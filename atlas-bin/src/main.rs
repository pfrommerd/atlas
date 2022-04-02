#![feature(try_blocks)]

use atlas_core::*;

use directories::ProjectDirs;
use rustyline::error::ReadlineError;
use rustyline::Editor;

use smol::LocalExecutor;
use futures_lite::future;

use std::rc::Rc;


use atlas_core::{Result, Error};
use atlas_core::parse::Lexer;
use atlas_core::grammar::ReplInputParser;

use atlas_core::store::{HeapStorage, Storage, Storable, value::Value};

use atlas_core::parse::ast::{ReplInput, Module, Span, DeclareModifier};

use atlas_core::compile::{Env, Compile};

use atlas_core::vm::{
    Machine, Resources,
    resource::{Snapshot, HttpProvider, BuiltinsProvider, FileProvider}
};
use crate::store::print::Depth;

use atlas_core::store::Handle;

use atlas_sandbox::{SandboxManager, ExecHandler};

use pretty::{BoxDoc, BoxAllocator};
use async_std::io;

fn interactive() -> Result<()> {
    env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("trace,rustyline=info")
    ).init();


    let dirs = ProjectDirs::from("org", "atlas", "atlas").unwrap();

    let mut rl = {
        let mut editor = Editor::<()>::new();
        std::fs::create_dir_all(dirs.config_dir()).unwrap();
        let path = dirs.config_dir().join("history.txt");
        editor.load_history(&path).ok();
        editor
    };

    let sandbox = dirs.runtime_dir().unwrap();
    let sm = SandboxManager::new(sandbox).unwrap();
    let exec_handler = Rc::new(ExecHandler::new(&sm));

    let mut env = Env::new();
    let storage = HeapStorage::new();

    let resources = {
        let mut resources = Resources::new();
        // Add the resource handlers
        resources.add_provider(Rc::new(FileProvider::new(&storage)));
        resources.add_provider(Rc::new(BuiltinsProvider::new(&storage)));
        resources.add_provider(Rc::new(HttpProvider::new(&storage)));
        Rc::new(resources)
    };

    let mut cache = Rc::new(storage.create_thunk_map());
    let mut snapshot = Rc::new(Snapshot::new(resources.clone()));


    // Load the prelude + __path__ into the env
    {
        let prelude_src = crate::core::prelude::PRELUDE;
        let mut dir_path = "file://".to_owned();
        dir_path.push_str(std::env::current_dir().unwrap().to_str().unwrap());
        dir_path.push_str("/");
        let dir_path = storage.insert_from(&Value::String(dir_path))?;
        env.insert(String::from("__path__"), dir_path);
        let lexer = crate::parse::Lexer::new(prelude_src);
        let parser = crate::grammar::ModuleParser::new();
        let module : crate::parse::ast::Module = parser.parse(lexer).unwrap();
        let expr = module.transpile();
        let prelude_compiled = expr.compile(&storage, &env)?
                                .store_in(&storage)?;
        let prelude_module = storage.insert_from(&Value::Thunk(prelude_compiled))?;

        let exec = LocalExecutor::new();
        future::block_on(exec.run(async {
            let mach = Machine::new(&storage, cache.clone(), snapshot.clone());
            mach.env_use(prelude_module, &mut env).await?;
            let r: Result<()> = Ok(());
            r
        }))?;
    }

    loop {
        let res = match rl.readline(">> ") {
            Err(ReadlineError::Interrupted) => continue,
            Err(ReadlineError::Eof) => return Ok(()),
            Err(_) => Err(Error::new_const(ErrorKind::IO, "Unable to read line")),
            Ok(s) => Ok(s)
        }?;
        rl.add_history_entry(res.as_str());
        if res.trim().is_empty() { continue; }

        let lexer = Lexer::new(&res);
        let parser = ReplInputParser::new();
        let result = parser.parse(lexer);
        let ast = match result {
            Ok(a) => a,
            Err(e) => {
                println!("Parse error {:?}", e);
                continue
            }
        };
        log::debug!("AST: {:?}", ast);

        match ast {
            ReplInput::Expr(expr) => {
                let res : Result<_> = try {
                    let core = expr.transpile();
                    log::debug!("Core: {:?}", core);
                    let compiled = core.compile(&storage, &env)?
                                            .store_in(&storage)?;
                    let thunk = storage.insert_from(&Value::Thunk(compiled))?;
                    let exec = LocalExecutor::new();
                    future::block_on(exec.run(async {
                        let mut mach = Machine::new(&storage, cache.clone(), snapshot.clone());
                        mach.add_syscall("exec", exec_handler.clone());
                        mach.force(&thunk).await
                    }))?
                };
                match res {
                    Err(e) => println!("{:?}", e),
                    Ok(handle) => {
                        let reader = handle.reader()?;
                        let doc : BoxDoc<'_, ()> = reader.pretty(Depth::Fixed(2), &BoxAllocator).into_doc();
                        println!("{}", doc.pretty(80));
                    }
                }
            },
            ReplInput::Decl(mut d) => {
                let res : Result<()> = try {
                    d.add_modifier(DeclareModifier::Pub);
                    let expr = Module{span: Span::new(0, 0), decl: vec![d]};
                    let core = expr.transpile();
                    log::debug!("Core: {:?}", core);
                    let compiled = core.compile(&storage, &env)?
                                            .store_in(&storage)?;
                    let thunk = storage.insert_from(&Value::Thunk(compiled))?;
                    let exec = LocalExecutor::new();
                    future::block_on(exec.run(async {
                        let mut mach = Machine::new(&storage, cache.clone(), snapshot.clone());
                        mach.add_syscall("exec", exec_handler.clone());
                        mach.env_use(thunk, &mut env).await
                    }))?
                };
                match res {
                    Err(e) => println!("{:?}", e),
                    _ => {}
                }
            },
            ReplInput::Command(cmd, expr) => {
                log::debug!("Cmd: {:?}", cmd);
                if cmd == "update_snapshot" {
                    print!("updating snapshot...");
                    snapshot = Rc::new(Snapshot::new(resources.clone()));
                    cache = Rc::new(storage.create_thunk_map());
                } else if cmd == "interact" {
                    // Manually launch a sandbox with the given argument
                    // as the underlying file system
                    let res : Result<()> = try {
                        let expr = expr.ok_or(Error::new("Must supply expr"))?;
                        let core = expr.transpile();
                        log::debug!("Core: {:?}", core);
                        let compiled = core.compile(&storage, &env)?
                                                .store_in(&storage)?;
                        let thunk = storage.insert_from(&Value::Thunk(compiled))?;
                        let mut mach = Machine::new(&storage, cache.clone(), snapshot.clone());
                        mach.add_syscall("exec", exec_handler.clone());
                        let sandbox = sm.create_sandbox(&mach, &thunk)?;
                        let exec = LocalExecutor::new();
                        future::block_on(exec.run(async {
                            // spawn a task which just handles sandbox requests
                            let stdin = io::stdin();
                            let mut buf = String::new();
                            future::or(async { sandbox.handle_requests().await; },
                                async { stdin.read_line(&mut buf).await.ok(); }).await;
                            let r : Result<()> = Ok(());
                            r
                        }))?;
                    };
                    match res {
                        Ok(()) => (),
                        Err(e) => println!("{:?}", e)
                    }
                } else {
                    println!("Command not recognized");
                }
            }
        }
        let path = dirs.config_dir().join("history.txt");
        rl.save_history(&path).ok();
    }
}

fn main() {
    interactive().unwrap();
}