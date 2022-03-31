use std::borrow::Borrow;

use std::path::{Path, PathBuf};
use std::fs;

use std::ffi::OsStr;
use std::ops::Deref;
use libc::ENOENT;


use atlas_core::store::{Storage, Handle, ObjectReader, RecordReader, BufferReader};
use atlas_core::vm::machine::Machine;
use atlas_core::vm::trace::Cache;
use atlas_core::{Error, ErrorKind};


pub struct SandboxManager {
    base: PathBuf,
}

impl SandboxManager {
    pub fn new(base: String) -> Result<Self, Error> {
        let path = Path::new(mount_point.as_str());
        fs::create_dir_all(path)
           .map_err(|_| Error::new("Failed to create sandbox"))?;
        Self { base: path }
    }

    pub fn create_sandbox<'s, 'm, C, S>(&self, m: &'m Machine<'s, C, S>) -> Sandbox<'s, C, S> 
                    where C: Cache<'s, S>, S: Storage {
        let u = Uuid::new_v4().to_string();
        let mut path = self.base.clone();
        path.push(u);
    }
}

struct Sandbox<'s, 'm, C, S> where C: Cache<'s, S>, S: Storage {
    session: 
}

impl<'s, C: Cache<'s, S>, S: Storage> Sandbox<'s, C, S> {
    pub fn new(base: PathBuf, h: S::Handle<'s>, m: &'m Machine<'s, C, S>) -> Self {

        create_dir(sandbox.upper_path().as_path());
        create_dir(sandbox.lower_path().as_path());
        create_dir(sandbox.overlay_path().as_path());

        let mut lower_dirs = Vec::<PathBuf>::new();
        lower_dirs.push(sandbox.lower_path());

        fuser::mount2()

        libmount::Overlay::writable(
            lower_dirs.iter().map(|p| p.as_path()), 
            sandbox.upper_path(), 
            sandbox.work_path(), 
            sandbox.overlay_path())
        .mount();

        sandbox.mount_atlasfs();

        sandbox
        let sandbox = Self {
            filesystem : AtlasFS::new(h, m),
            path
        };
    }

    fn mount_atlasfs(&self) {
        let options = vec![
            MountOption::RO, 
            MountOption::DefaultPermissions, 
        ];

        // fuser::mount2(self.filesystem, self.lower_path(), &options);
    } 
}