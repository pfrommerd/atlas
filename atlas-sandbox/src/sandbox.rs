use std::path::{Path, PathBuf};
use std::fs;


use atlas_core::store::Storage;
use atlas_core::vm::machine::Machine;
use atlas_core::{Error, Result};

use uuid::Uuid;

use super::atlasfs::AtlasFS;

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
    #[allow(dead_code)]
    atlasfs: AtlasFS<'m, 's, S>,
    #[allow(dead_code)]
    base_deleter : PathDeleter
}

impl<'h, 's, S: Storage +'s> Sandbox<'h, 's, S> {
    pub fn new(base: PathBuf, mach: &'h Machine<'s, S>, root: &'h S::Handle<'s>) -> Result<Self> {
        // Sandbox { base, atlasfs: AtlasFS }
        let mut input = base.clone();
        input.push("input");
        fs::create_dir_all(&input)
           .map_err(|_| Error::new("Failed to create sandbox"))?;
        let atlasfs = AtlasFS::new(input, 
                    String::from("sandbox"), mach, root.clone())?;
        Ok(Self {
            atlasfs,
            base_deleter: PathDeleter(base)
        })
    }

    pub async fn handle_requests(&self) {
        self.atlasfs.handle_requests().await
    }
}


struct PathDeleter(PathBuf);

impl Drop for PathDeleter {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}