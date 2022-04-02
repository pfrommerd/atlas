use atlas_core::vm::machine::SyscallHandler;
use atlas_core::store::Storage;
use atlas_core::vm::Machine;
use atlas_core::{Error, Result};

use crate::sandbox::SandboxManager;

use async_trait::async_trait;

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
        let _cmd_args = args.pop().unwrap();
        let _path = args.pop().unwrap();
        let _cwd = args.pop().unwrap();
        let fs = args.pop().unwrap();
        let _sandbox = self.sm.create_sandbox(mach, &fs)?;
        Ok(fs)
    }
}