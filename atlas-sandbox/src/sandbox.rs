use fuser::{Filesystem, Request, ReplyEntry, ReplyDirectory, ReplyData, FileAttr, FileType, ReplyAttr, MountOption};
use std::borrow::Borrow;
use std::{time::SystemTime};

use std::fs::create_dir;

use std::path::{Path, PathBuf};

use std::ffi::OsStr;
use std::ops::Deref;
use libc::{ENOENT};


use atlas_core::store::{Storage, Handle, ObjectReader, RecordReader, BufferReader};
use atlas_core::vm::machine::Machine;
use atlas_core::vm::trace::Cache;
use atlas_core::{Error, ErrorKind};

use bimap::hash::BiHashMap;


type Inode = u64;

pub struct AtlasFS<'s, C: Cache<'s, S>, S: Storage> {
    root_handle: S::Handle<'s>,
    machine: Machine<'s, C, S>,
    timestamp: SystemTime,
    inode_map: BiHashMap<Inode, S::Handle<'s>>,
    inode_counter: u64
}

pub struct Sandbox<'s, C: Cache<'s, S>, S: Storage>{
    filesystem: AtlasFS<'s, C, S>,
    path: String
}

impl<'s, C: Cache<'s, S>, S: Storage> Sandbox<'s, C, S> {
    pub fn new(path: String, h: S::Handle<'s>, m: Machine<'s, C, S>) -> Self {

        let sandbox = Self {
            filesystem : AtlasFS::new(h, m),
            path
        };

        create_dir(sandbox.upper_path().as_path());
        create_dir(sandbox.lower_path().as_path());
        create_dir(sandbox.overlay_path().as_path());

        let mut lower_dirs = Vec::<PathBuf>::new();
        lower_dirs.push(sandbox.lower_path());

        libmount::Overlay::writable(
            lower_dirs.iter().map(|p| p.as_path()), 
            sandbox.upper_path(), 
            sandbox.work_path(), 
            sandbox.overlay_path())
        .mount();

        sandbox.mount_atlasfs();

        sandbox
    }

    fn lower_path(&self) -> PathBuf {
        let mut lower_path = PathBuf::from(self.path.as_str());
        lower_path.push("lower");
        lower_path
    }

    fn upper_path(&self) -> PathBuf {
        let mut lower_path = PathBuf::from(self.path.as_str());
        lower_path.push("upper");
        lower_path
    }

    fn overlay_path(&self) -> PathBuf {
        let mut lower_path = PathBuf::from(self.path.as_str());
        lower_path.push("overlay");
        lower_path
    }

    fn work_path(&self) -> PathBuf {
        let mut lower_path = PathBuf::from(self.path.as_str());
        lower_path.push("work");
        lower_path
    }


    fn mount_atlasfs(&self) {
        let options = vec![
            MountOption::RO, 
            MountOption::DefaultPermissions, 
        ];

        // fuser::mount2(self.filesystem, self.lower_path(), &options);
    } 
}

impl<'s, C: Cache<'s, S>, S: Storage> AtlasFS<'s, C, S> {
    pub fn new(h: S::Handle<'s>, m: Machine<'s, C, S>) -> Self {
        Self {
            root_handle: h,
            machine: m,
            timestamp: SystemTime::now(),
            inode_map: BiHashMap::new(),
            inode_counter: 0
        }
    }
}

impl<'s, C: Cache<'s, S>, S: Storage> Filesystem for AtlasFS<'s, C, S> {
    /// Look up a directory entry by name and get its attributes.
    fn lookup(&mut self, _req: &Request, parent: u64, name: &OsStr, reply: ReplyEntry) {
        todo!()
    }


    /// Read directory.
    /// Send a buffer filled using buffer.fill(), with size not exceeding the
    /// requested size. Send an empty buffer on end of stream. fh will contain the
    /// value set by the opendir method, or will be undefined if the opendir method
    /// didn't set any value.
    fn readdir(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        fh: u64,
        offset: i64,
        mut reply: ReplyDirectory,
    ) {
        if ino != 1 {
            reply.error(ENOENT);
            return;
        }

        let entries = vec![
            (1, FileType::Directory, "."),
            (1, FileType::Directory, ".."),
            (2, FileType::RegularFile, "hello.txt"),
        ];

        for (i, entry) in entries.into_iter().enumerate().skip(offset as usize) {
            // i + 1 means the index of the next entry
            if reply.add(entry.0, (i + 1) as i64, entry.1, entry.2) {
                break;
            }
        }
        reply.ok();
    }

    /// Read data.
    /// Read should send exactly the number of bytes requested except on EOF or error,
    /// otherwise the rest of the data will be substituted with zeroes. An exception to
    /// this is when the file has been opened in 'direct_io' mode, in which case the
    /// return value of the read system call will reflect the return value of this
    /// operation. fh will contain the value set by the open method, or will be undefined
    /// if the open method didn't set any value.
    ///
    /// flags: these are the file flags, such as O_SYNC. Only supported with ABI >= 7.9
    /// lock_owner: only supported with ABI >= 7.9
    fn read(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        fh: u64,
        offset: i64,
        size: u32,
        flags: i32,
        lock_owner: Option<u64>,
        reply: ReplyData,
    ) {
        let res : Result<(), Error> = try {
            let handle = self.inode_map.get_by_left(&ino)
                            .ok_or(Error::new_const(ErrorKind::BadPointer, "Bad handle!"))?;
            
            let record_reader = handle.reader()?.as_record()?;
            let contents = record_reader.get("contents")?;
            let contents_reader = contents.borrow().reader()?.as_buffer()?;
            let data = contents_reader.slice(offset as usize, size as usize);
            reply.data(&data);
        };

        match res {
            Ok(data) => (),
            Err(e) => panic!("idk")
        }
    }

    fn getattr(&mut self, _req: &Request, ino: u64, reply: ReplyAttr) {
        // let res : Result<(), Error> = try {

        // }
    }

}