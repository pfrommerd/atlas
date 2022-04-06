use atlas_core::vm::machine::SyscallHandler;
use atlas_core::store::{Storage, Handle, ObjectReader, StringReader, ReaderWhich};
use atlas_core::vm::Machine;
use atlas_core::{Error, Result};

use crate::sandbox::SandboxManager;

use async_trait::async_trait;
use std::ops::Deref;
use std::borrow::Borrow;

// use smol::Timer;
// use std::time::Duration;
// use futures_lite::future;

pub struct ExecHandler<'sm> {
    sm: &'sm SandboxManager
}

impl<'sm> ExecHandler<'sm> {
    pub fn new(sm: &'sm SandboxManager) -> Self {
        Self { sm }
    }
}

#[async_trait(?Send)]
impl<'sm, 's, S: Storage + 's> SyscallHandler<'s, S> for ExecHandler<'sm> {
    async fn call(&self, _sys: &str, mach: &Machine<'s, S>, mut args: Vec<S::Handle<'s>>) 
            -> Result<S::Handle<'s>> {
        // Call syntax is fs, cwd, path, args
        if args.len() != 4 {
            return Err(Error::new("Wrong number of arguments to exec call"));
        }
        let mut cmd_args = args.pop().unwrap();
        let path = args.pop().unwrap();
        let cwd = args.pop().unwrap();
        let fs = args.pop().unwrap();

        let path_reader = path.reader()?.as_string()?;
        let cwd_reader = cwd.reader()?.as_string()?;
        let path = path_reader.as_slice();
        let cwd = cwd_reader.as_slice();

        let sandbox = self.sm.create_sandbox(mach, &fs)?;
        let mut args = Vec::new();
        loop {
            let args_forced = mach.force(&cmd_args).await?;
            use ReaderWhich::*;
            match args_forced.reader()?.which() {
                Nil => break,
                Cons(h, tail) => {
                    let s = h.borrow().reader()?.as_string()?;
                    let s = s.as_slice();
                    args.push(s.to_string());
                    cmd_args = tail.borrow().clone();
                }
                _ => return Err(Error::new("Arguments expected to be in a list"))
            };
        }
        let args : Vec<&str> = args.iter().map(|x| x.deref()).collect();
        sandbox.exec(cwd.deref(), path.deref(), args.deref()).await?;
        sandbox.consume().await
    }
}