use std::path::{Path, PathBuf};
use std::fs;


use atlas_core::store::{Storage, Handle, ObjectReader, RecordReader, StringReader};
use atlas_core::store::value::Value;
use atlas_core::vm::machine::Machine;
use atlas_core::{Error, Result};

use futures_lite::future;

use uuid::Uuid;

use super::atlasfs::AtlasFS;
use async_process::{Command, unix::CommandExt};

use nix::sched::CloneFlags;
use nix::fcntl::OFlag;
use nix::mount::MsFlags;

use std::ffi::CStr;
use bytes::Bytes;
use std::borrow::Borrow;

use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::FileTypeExt;

pub struct SandboxManager {
    base: PathBuf,
}

impl SandboxManager {
    pub fn new(base: &Path) -> Result<Self> {
        fs::create_dir_all(base)
           .map_err(|_| Error::new("Failed to create sandbox manager"))?;
        Ok(Self { base: base.to_path_buf() })
    }

    pub fn create_sandbox<'h, 's, S: Storage>(&self, mach: &'h Machine<'s, S>, root: &'h S::Handle<'s>) 
                -> Result<Sandbox<'h, 's, S>> {
        let u = Uuid::new_v4().to_string();
        let mut path = self.base.clone();
        path.push(u);
        Sandbox::new(path, mach, root)
    }
}

pub struct Sandbox<'m, 's, S : Storage + 's> {
    atlasfs_path: PathBuf,
    overlay_path: PathBuf,
    diff_path: PathBuf,
    workdir_path: PathBuf,
    root: &'m S::Handle<'s>,
    machine: &'m Machine<'s, S>,
    _atlasfs: AtlasFS<'m, 's, S>,
    _base_deleter : PathDeleter
}

impl<'h, 's, S: Storage +'s> Sandbox<'h, 's, S> {
    pub fn new(base: PathBuf, machine: &'h Machine<'s, S>, root: &'h S::Handle<'s>) -> Result<Self> {
        let mut atlasfs_path = base.clone();
        atlasfs_path.push("atlasfs");
        let mut diff_path = base.clone();
        diff_path.push("diff");
        let mut overlay_path = base.clone();
        overlay_path.push("overlay");
        let mut  workdir_path = base.clone();
        workdir_path.push("work");

        fs::create_dir_all(&atlasfs_path)
           .map_err(|_| Error::new("Failed to create sandbox"))?;
        fs::create_dir_all(&diff_path)
           .map_err(|_| Error::new("Failed to create sandbox"))?;
        fs::create_dir_all(&overlay_path)
           .map_err(|_| Error::new("Failed to create sandbox"))?;
        fs::create_dir_all(&workdir_path)
           .map_err(|_| Error::new("Failed to create sandbox"))?;
        // Setup the base atlasfs filesystem
        let atlasfs = AtlasFS::new(&atlasfs_path,
                    String::from("sandbox"), machine, root.clone())?;
        // Setup the overlay mount
        Ok(Self {
            atlasfs_path,
            diff_path,
            overlay_path,
            workdir_path,
            root,
            machine,
            _atlasfs: atlasfs,
            _base_deleter: PathDeleter(base)
        })
    }

    pub async fn handle_requests(&self) {
        self._atlasfs.handle_requests().await
    }

    pub async fn exec(&self, cwd: &str, cmd: &str, args: &[&str]) -> Result<()> {
        let cwd = cwd.to_string();
        let atlasfs_path = self.atlasfs_path.clone();
        let diff_path = self.diff_path.clone();
        let workdir_path = self.workdir_path.clone();
        let root_path = self.overlay_path.clone();

        let real_euid = nix::unistd::geteuid();
        let real_egid = nix::unistd::getegid();
        let umap_line = format!("0 {real_euid} 1");
        let gmap_line = format!("0 {real_egid} 1");

        let mut cmd = Command::new(cmd);
        cmd.env_clear()
            .env("USER", "root")
            .env("HOME", "/root")
           .env("SHELL", "/usr/bin/bash").args(args);
        // TODO: Make the namespace persistent
        // and mount into that namespace
        unsafe { cmd.pre_exec(move || {
            // first we do another fork to set the id map
            let mut flags = CloneFlags::empty();
            flags.insert(CloneFlags::CLONE_NEWNS);
            flags.insert(CloneFlags::CLONE_NEWUSER);
            nix::sched::unshare(flags)?;

            // Overrite the proc/self/setgroups
            // and write the uid maps
            let fd = nix::fcntl::open("/proc/self/setgroups", OFlag::O_WRONLY, nix::sys::stat::Mode::empty())?;
            nix::unistd::write(fd, "deny".as_bytes())?;
            nix::unistd::close(fd)?;
            let fd = nix::fcntl::open("/proc/self/uid_map", OFlag::O_WRONLY, nix::sys::stat::Mode::empty())?;
            nix::unistd::write(fd, umap_line.as_bytes())?;
            nix::unistd::close(fd)?;
            let fd = nix::fcntl::open("/proc/self/gid_map", OFlag::O_WRONLY, nix::sys::stat::Mode::empty())?;
            nix::unistd::write(fd, gmap_line.as_bytes())?;
            nix::unistd::close(fd)?;

            // switch the user!
            nix::unistd::setuid(nix::unistd::Uid::from_raw(0))?;
            nix::unistd::setgid(nix::unistd::Gid::from_raw(0))?;

            let mut options = Vec::new();
            options.extend(b"lowerdir=");
            append_escape(&mut options, atlasfs_path.as_path());
            options.extend(b",upperdir=");
            append_escape(&mut options, diff_path.as_path());
            options.extend(b",workdir=");
            append_escape(&mut options, workdir_path.as_path());
            options.extend(b",userxattr");
            nix::mount::mount(
                Some(CStr::from_bytes_with_nul(b"overlay\0").unwrap()),
                &root_path,
                Some(CStr::from_bytes_with_nul(b"overlay\0").unwrap()),
                MsFlags::empty(),
                Some(&*options)

            ).map_err(|x| {
                println!("Failed to mount overlay {x}");
                std::io::Error::new(std::io::ErrorKind::Unsupported, "Unable to mount")
            })?;

            // chroot into the new root
            nix::unistd::chroot(root_path.as_path())?;
            nix::unistd::chdir(cwd.as_str())?;
            Ok(())
        }) };
        // Wait for the child to finish and handle requests at the same time
        let r: Result<()> = future::or(async { 
            // We need to run the spawn in a new thread,
            // or after the chroot syscall will hang trying to access
            // something in the filesystem
            // and not let us handle the coresponding IO request
            let mut child = blocking::unblock(move || { cmd.spawn() }).await?;
            child.status().await.ok();
            Ok(())
        }, async { 
            self.handle_requests().await;
            Ok(()) 
        }).await;
        r
    }

    pub async fn consume(self) -> Result<S::Handle<'s>> {
        merge_overlay_dirs(Some(self.root.clone()), self.diff_path.as_path(), self.machine).await
    }
}

struct PathDeleter(PathBuf);

impl Drop for PathDeleter {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

fn append_escape(dest: &mut Vec<u8>, path: &Path) {
    for &byte in path.as_os_str().as_bytes().iter() {
        match byte {
            // This is escape char
            b'\\' => { dest.push(b'\\'); dest.push(b'\\'); }
            // This is used as a path separator in lowerdir
            b':' => { dest.push(b'\\'); dest.push(b':'); }
            // This is used as a argument separator
            b',' => { dest.push(b'\\'); dest.push(b','); }
            x => dest.push(x),
        }
    }
}

use std::collections::HashMap;

#[async_recursion::async_recursion(?Send)]
async fn merge_overlay_dirs<'s, S: Storage + 's>(old_dir: Option<S::Handle<'s>>, 
                    overlay_dir: &Path, mach: &Machine<'s, S>) -> Result<S::Handle<'s>> {
    let is_empty = std::fs::read_dir(overlay_dir)?.next().is_none();
    if is_empty && old_dir.is_some() {
        if let Some(old_dir) = old_dir {
            Ok(old_dir.clone())
        } else {
            panic!("Unexpected")
        }
    } else {
        // Map from entry name to (k, v)
        let mut entries : HashMap<String, S::Handle<'s>> = match old_dir {
            None => HashMap::new(),
            Some(old_dir) => {
                let old_rec = mach.force(&old_dir).await?;
                let old_rec = old_rec.reader()?.as_record()?;
                let old_entries = old_rec.get("entries")?;
                let old_entries = mach.force(old_entries.borrow()).await?;
                let old_entries = old_entries.reader()?.as_record()?;
                let mut map = HashMap::new();
                for (k, v) in old_entries.iter() {
                    let ks = k.borrow().reader()?.as_string()?;
                    let ks = ks.as_slice();
                    map.insert(ks.to_string(), v.borrow().clone());
                }
                map
            }
        };
        let store = mach.store();
        // traverse the overlay directory and for each item
        // replace or merge in the entries
        for overlay_entry in std::fs::read_dir(overlay_dir)? {
            let overlay_entry = overlay_entry?;
            let name = overlay_entry.file_name().to_str().ok_or(Error::new("Not a valid filename"))?.to_string();
            let mut sub_path = overlay_dir.to_path_buf();
            sub_path.push(&name);
            let ft = overlay_entry.file_type()?;
            if ft.is_dir() {
                // check if it is an "opaque" directory
                let old_dir = match entries.get(overlay_entry.file_name().to_str().unwrap()) {
                    Some(v) => {
                        let e = mach.force(&v).await?;
                        let e = e.reader()?.as_record()?;
                        let is_dir = e.get("entries").is_ok();
                        if is_dir {
                            Some(v.clone())
                        } else {
                            None
                        }
                    },
                    None => None
                };
                let sub_merged = merge_overlay_dirs(old_dir, &sub_path, mach).await?;
                entries.insert(name, sub_merged);
            } else if ft.is_file() {
                // Read in the file
                let data = std::fs::read(&sub_path).map_err(|_| Error::new("Unable to read overlay file"))?;
                let val = Value::Buffer(Bytes::from(data));
                let val = store.insert_from(&val)?;
                let content_str = store.insert_from(&Value::String(String::from("content")))?;
                let entry = store.insert_from(&Value::Record(vec![(content_str, val)]))?;
                entries.insert(name, entry);
            } else if ft.is_char_device() {
                // This is a "whiteout" block device
                entries.remove(&name);
            } else {
                return Err(Error::new("Unrecognized overlay file type"));
            }
        }
        // insert as {"entries": {...}}
        let entries : Result<Vec<(S::Handle<'s>, S::Handle<'s>)>> = entries.into_iter().map(|(k,v)| {
            let r: Result<(S::Handle<'s>, S::Handle<'s>)> = try { 
                (store.insert_from(&Value::String(k))?, v)
            };
            r
        }).collect();
        let entries = store.insert_from(&Value::Record(entries?))?;
        let entries_str = store.insert_from(&Value::String(String::from("entries")))?;
        store.insert_from(&Value::Record(vec![(entries_str, entries)]))
    }
}