#![allow(async_fn_in_trait)]

pub mod fs;
pub mod fuse;
pub mod util;

use fs::FileSystem;
pub use std::io::Error;

trait Sandbox {
    type FileSystem : FileSystem;
    // get the filesystem
    fn fs<'s>(&'s self) -> Result<&'s Self::FileSystem, Error>;
}