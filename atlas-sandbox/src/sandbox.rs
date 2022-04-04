use std::path::{Path, PathBuf};
use std::fs;


use atlas_core::store::Storage;
use atlas_core::vm::machine::Machine;
use atlas_core::{Error, Result};

use futures_lite::future;

use uuid::Uuid;

use super::atlasfs::AtlasFS;
use async_process::Command;

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
    combined_path: PathBuf,
    _atlasfs: AtlasFS<'m, 's, S>,
    _base_deleter : PathDeleter
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
        let mut combined_path = base.clone();
        combined_path.push("input");
        Ok(Self {
            combined_path,
            _atlasfs: atlasfs,
            _base_deleter: PathDeleter(base)
        })
    }

    pub async fn handle_requests(&self) {
        self._atlasfs.handle_requests().await
    }

    pub async fn exec(&self, _cwd: &str, cmd: &str, args: &[&str]) -> Result<()> {
        let mut c = Command::new("unshare");
        c.arg("--mount").arg("--fork").arg("--user").arg("--map-root-user")
           .arg("--root").arg(&self.combined_path)
           .arg(cmd).args(args).env_clear()
           .env("PATH", "/bin:/usr/bin")
           .env("USER", "root")
           .env("HOME", "/root")
           .env("SHELL", "/usr/bin/bash");
        log::info!("Running sandbox cmd: {:?}", c);
        let mut child = c.spawn()?;
        future::or(async { child.status().await.ok();}, 
            self.handle_requests()).await;
        Ok(())
    }
}


struct PathDeleter(PathBuf);

impl Drop for PathDeleter {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}