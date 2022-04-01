use std::path::{Path, PathBuf};
use std::fs;


use atlas_core::store::Storage;
use atlas_core::vm::machine::Machine;
use atlas_core::Error;

use uuid::Uuid;

use super::atlasfs::AtlasFS;

pub struct SandboxManager {
    base: PathBuf,
}

impl SandboxManager {
    pub fn new(base: &Path) -> Result<Self, Error> {
        fs::create_dir_all(base)
           .map_err(|_| Error::new("Failed to create sandbox"))?;
        Ok(Self { base: base.to_path_buf() })
    }

    pub fn create_sandbox<'h, 's, S: Storage>(&self, mach: &'h Machine<'s, S>, root: &'h S::Handle<'s>) -> Sandbox<'h, 's, S> {
        let u = Uuid::new_v4().to_string();
        let mut path = self.base.clone();
        path.push(u);
        todo!()
    }
}

pub struct Sandbox<'m, 's, S : Storage + 's> {
    base : PathBuf,
    atlasfs: AtlasFS<'m, 's, S>
}

impl<'h, 's, S: Storage +'s> Sandbox<'h, 's, S> {
    pub fn new(base: PathBuf, mach: &'h Machine<'s, S>, root: &'h S::Handle<'s>) -> Self {
        // Sandbox { base, atlasfs: AtlasFS }
        todo!();
    }
}