use directories::ProjectDirs;
use rustyline::error::ReadlineError;
use rustyline::Editor;

use atlas::value::mem::MemoryAllocator;
use atlas::value::{Env, ObjHandle};
use atlas::vm::{Machine, ForceCache};
use atlas::error::Error;
use atlas::parse;
use atlas::parse::ast::{ReplInput, Module, Span, DeclareModifier};
use atlas::compile::Compile;
use atlas::grammar;

use smol::LocalExecutor;
use futures_lite::future;

fn interactive() {
    env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("trace,rustyline=info")
    ).init();
    //let mut heap = Heap::new();
    // create a default node environment
    //let mut nenv = NodeEnv::default(&mut heap);
    //let mut sym_env = SymbolEnv::default();

    let mut rl = Editor::<()>::new();
    let dir = ProjectDirs::from("org", "atlas", "atlas");
    if let Some(d) = &dir {
        std::fs::create_dir_all(d.config_dir()).unwrap();
        let path = d.config_dir().join("history.txt");
        rl.load_history(&path).ok();
    }

    let alloc = MemoryAllocator::new();
    let cache = ForceCache::new();
    let mut env = Env::new();
    atlas::vm::populate_prelude(&alloc, &mut env).unwrap();

    loop {
        let res = rl.readline(">> ");
        let input = match res {
            Err(ReadlineError::Interrupted) => continue,
            Err(ReadlineError::Eof) => break,
            Err(_) => { panic!("Error while reading line") }
            Ok(s) => { rl.add_history_entry(s.as_str()); s
            }
        };
        if input.trim().len() == 0 {
            continue;
        }

        let lexer = parse::Lexer::new(&input);
        let parser = grammar::ReplInputParser::new();
        let result = parser.parse(lexer);
        let repl_input = match result {
            Err(e) => {
                println!("Parsing error: {:?}", e);
                continue;
            }
            Ok(repl_input) => repl_input,
        };
        log::debug!("AST: {:?}", repl_input);
        let exec = LocalExecutor::new();
        let mach = Machine::new(&alloc, &cache);
        let res = future::block_on(exec.run(async {
            match repl_input {
                ReplInput::Expr(expr) => {
                    let core = expr.transpile();
                    log::debug!("Core : {:?}", core);
                    let res = core.compile(&alloc, &env)?;
                    let res = mach.force(res).await?;
                    println!("{:?}", res.to_owned());
                },
                ReplInput::Decl(mut d) => {
                    d.add_modifier(DeclareModifier::Pub);
                    let expr = Module{  
                        span: Span::new(0, 0), decl: vec![d]
                    };
                    let core = expr.transpile();
                    log::debug!("core : {:?}", core);
                    let mod_handle = core.compile(&alloc, &env)?;
                    mach.env_use(mod_handle, &mut env).await?;
                },
                ReplInput::Pointer(p) => {
                    let r = unsafe { ObjHandle::new(&alloc, p) };
                    println!("{:?}", r.to_owned())
                }
            }
            Ok::<(), Error>(())
        }));
        if let Err(e) = res {
            println!("Error: {:?}", e)
        }
    }
    if let Some(d) = &dir {
        let path = d.config_dir().join("history.txt");
        rl.save_history(&path).ok();
    }
}

fn main() {
    interactive();
}