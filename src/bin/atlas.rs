use clap::{App, Arg, ArgMatches, SubCommand};
use directories::ProjectDirs;
use rustyline::error::ReadlineError;
use rustyline::Editor;

use atlas::grammar;
use atlas::core::lang::{ExprBuilder};
use atlas::util::{PrettyReader};
use atlas::parse::ast::{Span, Expr, Declarations, ReplInput};
use atlas::parse::lexer::Lexer;

use atlas::value::{local::LocalStorage, Storage, ObjectRef, ValueRef, ExtractValue};
use atlas::optim::*;
use atlas::optim::transpile::*;
use atlas::vm::{tracer::ForceCache, machine::Machine};
use atlas::core::lang::SymbolMap;
use smol::LocalExecutor;

use futures_lite::future;

fn eval_expr<'s, S: Storage>(store: &'s S, env: &Env<'s, S>, expr: &Expr<'_>) -> S::ObjectRef<'s> {
    // First transpile to core
    let mut m = capnp::message::Builder::new_default();
    let mut cexp = m.init_root::<ExprBuilder>();
    expr.transpile(&env.symbol_map(), cexp.reborrow());
    let exp = cexp.into_reader();
    println!("{}", exp.pretty_render(80));
    // Do the graph shit
    let code_thunk = exp.transpile(store, env).unwrap();
    // Now create the machine
    let cache = ForceCache::new();
    let machine  = Machine::new(store, &cache);

    // The executor
    let exec = LocalExecutor::new();
    future::block_on(exec.run(async {
        machine.force(code_thunk).await.unwrap()
    }))
}

fn use_module<'s, S: Storage>(store: &'s S, env: &mut Env<'s, S>, module: &Expr<'_>) {
    let _val = eval_expr(store, env, module);
    let rec = _val.value().unwrap().reader().record().unwrap();
    for (k, v) in rec {
        let k = store.get(k).unwrap();
        let k = k.value().unwrap();
        let key = k.reader().str().unwrap();
        env.insert(key.to_owned(), store.get(v).unwrap());
        println!("inserting {} -> {} into environment", key, v)
    }
}


fn run(args: &ArgMatches) {
    let input_file = args.value_of("INPUT").unwrap();
    let contents = std::fs::read_to_string(input_file).expect("No such file");
    let lexer = Lexer::new(&contents);
    let parser = grammar::ModuleParser::new();
    let result = parser.parse(lexer);
    let parsed_exprssion = match result {
        Ok(p) => p,
        Err(e) => {
            println!("{:?}", e);
            panic!("Error parsing input module!")
        }
    };
    let mut m = capnp::message::Builder::new_default();
    let mut cexp = m.init_root::<ExprBuilder>();
    parsed_exprssion.transpile(&SymbolMap::new(), cexp.reborrow());
    println!("Parse: {:?}", parsed_exprssion);
    println!("Core: {}", cexp.into_reader().pretty_render(80));
}

fn interactive(args: &ArgMatches) {
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


    // The state
    let store = LocalStorage::new_default();
    let mut env = Env::new();

    // First load in the prelude
    let prelude = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/prelude/prelude.at"));
    let prelude_expr = {
        let lexer = Lexer::new(&prelude);
        let parser = grammar::ModuleParser::new();
        parser.parse(lexer).unwrap()
    };

    use_module(&store, &mut env, &prelude_expr);

    // Load in the ops prelude
    let ops = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/prelude/ops.at"));
    let ops_expr = {
        let lexer = Lexer::new(&ops);
        let parser = grammar::ModuleParser::new();
        parser.parse(lexer).unwrap()
    };
    use_module(&store, &mut env, &ops_expr);

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

        let lexer = Lexer::new(&input);
        let parser = grammar::ReplInputParser::new();
        let result = parser.parse(lexer);

        if args.is_present("parse") {
            println!("Parse: {:?}", result);
        }
        let repl_input = match result {
            Err(e) => {
                println!("Parsing error: {:?}", e);
                continue;
            }
            Ok(repl_input) => repl_input,
        };
        match repl_input {
            ReplInput::Expr(exp) => {
                let v = eval_expr(&store, &env, &exp);
                println!("{}", v.value().unwrap().reader().pretty_render(80));
            },
            ReplInput::Decl(mut d) => {
                d.set_public(true);
                let expr = Expr::Module(Declarations { span: Span::new(0, 0), declarations: vec![d]});
                use_module(&store, &mut env, &expr);
            },
            ReplInput::Pointer(p) => {
                let r = store.get(p);
                match r {
                    Ok(s) => {
                        println!("{}", s.value().unwrap().reader().pretty_render(80))
                    },
                    _  => {
                        println!("Address not found")
                    }
                }
            }
        }
    }
    if let Some(d) = &dir {
        let path = d.config_dir().join("history.txt");
        rl.save_history(&path).ok();
    }
}

fn main() {
    let matches = App::new("Atlas Build System")
        .version("pre-alpha")
        .author("Daniel Pfrommer <dan.pfrommer@gmail.com>")
        .about("A cutting-edge build system")
        .subcommand(SubCommand::with_name("run").arg(Arg::with_name("INPUT").required(true)))
        .subcommand(
            SubCommand::with_name("interactive")
                .arg(Arg::with_name("parse").short("p").help("Print parse tree"))
                .arg(Arg::with_name("step").short("s").help("Step through evals"))
                .arg(Arg::with_name("core").short("c").help("Core Expression"))
                .about("interactive REPL input"),
        )
        .get_matches();

    if let Some(args) = matches.subcommand_matches("interactive") {
        interactive(args);
    } else if let Some(args) = matches.subcommand_matches("run") {
        run(args);
    } else {
        println!("Taking a nap....no command specified");
    }
}
