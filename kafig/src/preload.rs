use std::{marker::PhantomData, path::PathBuf};

use kafig_core::{Sandbox, Process, ExitStatus, Command, Result};

pub struct PreloadProcess<'s> {
    child: async_process::Child,
    _phantom: PhantomData<&'s ()>
}

impl<'s> Process for PreloadProcess<'s> {
    async fn wait(&mut self) -> Result<ExitStatus> {
        self.child.status().await
    }
}

pub struct PreloadSandbox {
    preload_lib_path: PathBuf
}

impl PreloadSandbox {
    pub fn new<P: ToOwned<Owned=PathBuf>>(preload_lib_path: P) -> Self {
        Self { preload_lib_path: preload_lib_path.to_owned() }
    }

    pub fn new_auto_lib() -> Self {
        let preload_lib_path = "target/debug/libkafig_preload.so";
        Self { preload_lib_path: preload_lib_path.to_owned().into() }
    }
}

impl Sandbox for PreloadSandbox {
    type Process<'s> = PreloadProcess<'s> where Self : 's;

    async fn spawn<'s>(&'s self, command: &Command) -> Result<PreloadProcess> {
        let child = async_process::Command::new(command.get_program())
                        .args(command.get_args()).env_clear()
                        .env("LD_PRELOAD", &self.preload_lib_path)
                        .env("KAFIG_FAKEROOT_SOCKET", "").spawn()?;
        Ok(PreloadProcess {
            child, _phantom: PhantomData
        })
    }
}