use std::path::{Path, PathBuf};

use super::sandbox::Sandbox;
use uuid::Uuid;
use atlas_core::store::Storage;
use std::fs::create_dir;

pub struct SandboxManager {
    mount_point: String,
}

impl SandboxManager {
    pub fn new(mount_point: String) -> Self {
        let path = Path::new(mount_point.as_str());
        match create_dir(path) {
            _ => SandboxManager {mount_point}
        }
    }

    pub fn create_sandbox(&self) {
        let u = Uuid::new_v4().to_string();
        let mut path = PathBuf::from(&self.mount_point);
        path.push(u);
    }
}