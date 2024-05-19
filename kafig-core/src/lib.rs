#![allow(async_fn_in_trait)]

pub mod fs;
pub mod util;

use std::ffi::{OsStr, OsString};
pub use std::io::{Error, Result};
pub use std::process::ExitStatus;

pub use fs::FileSystem;

pub struct Command {
    program: OsString,
    args: Vec<OsString>,
    env: Vec<(OsString, OsString)>
}

impl Command {
    pub fn new<S: AsRef<OsStr>>(program: S) -> Self {
        Command { program : program.as_ref().to_owned(), args: Default::default(), env: Default::default() }
    }

    pub fn arg<S: AsRef<OsStr>>(&mut self, arg: S) -> &mut Self {
        self.args.push(arg.as_ref().to_owned());
        self
    }
    pub fn args<I: IntoIterator<Item=S>, S: AsRef<OsStr>>(&mut self, args: I) -> &mut Self {
        self.args.extend(args.into_iter().map(|x| x.as_ref().to_owned()));
        self
    }

    pub fn get_program(&self) -> &OsStr { &self.program }
    pub fn get_args<'s>(&'s self) -> impl Iterator<Item=&'s OsStr> {
        self.args.iter().map(|x| x.as_os_str())
    }
    pub fn get_env<'s>(&'s self) -> impl Iterator<Item=(&'s OsStr, &'s OsStr)> {
        self.env.iter().map(|(e, v)| (e.as_os_str(), v.as_os_str()))
    }
}

pub trait Process {
    async fn wait(&mut self) -> Result<ExitStatus>;
}

pub trait Sandbox {
    // type FileSystem<'s> : FileSystem where Self : 's;
    type Process<'s> : Process where Self : 's;

    // fn fs<'s>(&'s self) -> &'s Self::FileSystem;
    async fn spawn<'s>(&'s self, command: &Command) -> Result<Self::Process<'s>>;
}