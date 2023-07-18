#![feature(async_fn_in_trait)]
#![feature(impl_trait_projections)]
pub mod fs;
pub mod fuse;

use fs::FileSystem;
pub use std::io::Error;

trait Sandbox {
    type FileSystem : FileSystem;
    // get the filesystem
    fn fs<'s>(&'s self) -> Result<&'s Self::FileSystem, Error>;
}