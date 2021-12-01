extern crate clap;
extern crate directories;
extern crate rustyline;

use clap::{App, Arg, ArgMatches, SubCommand};
use directories::ProjectDirs;
use rustyline::error::ReadlineError;
use rustyline::Editor;
use std::rc::Rc;

use atlas::core;
use atlas::grammar;
use atlas::parse::ast::{ReplInput};
use atlas::parse::lexer::Lexer;
use atlas::vm::machine::Machine;
use atlas::vm::op::{CodeBuilder, Op};
use atlas::vm::value::{Heap, Scope};

fn run(args: &ArgMatches) {
    let input_file = args.value_of("INPUT").unwrap();
    let contents = std::fs::read_to_string(input_file).expect("No such file");
    let lexer = Lexer::new(&contents);
    let parser = grammar::ModuleParser::new();
    let result = parser.parse(lexer);
    let parsed = match result {
        Ok(p) => p,
        Err(e) => {
            println!("{:?}", e);
            panic!("Error parsing input module!")
        }
    };
    println!("Parse: {:?}", parsed);
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

    let mut heap = Heap::new();
    let machine = Machine::new(&mut heap);

    let sym_map = core::builtin::symbols();
    let mut cb = CodeBuilder::new();
    let mut sb = cb.next();
    let segment_id = sb.id;

    //let env = vm::builtin::prelude(&mut sb, &mut cb);
    sb.append(Op::Done);

    cb.register(sb);

    let (builtin_code, segment_locs) = cb.build();
    println!("Code: {:?}", builtin_code);
    let mut builtin_loc = segment_locs[segment_id];

    // run builtin_code
    let builtin_code = Rc::new(builtin_code);
    let mut scope = Scope::new();
    Machine::exec_scope(machine.heap, &builtin_code, &mut builtin_loc, &mut scope);

    // scope now contains all of the builtins!

    loop {
        let res = rl.readline(">> ");
        let input = match res {
            Err(ReadlineError::Interrupted) => continue,
            Err(ReadlineError::Eof) => break,
            Err(err) => {
                println!("Readline Error: {:?}", err);
                break;
            }
            Ok(s) => {
                rl.add_history_entry(s.as_str());
                s
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
            ReplInput::Expr(ast) => {
                let core_expr = ast.transpile(&sym_map);
                if args.is_present("core") {
                    println!("Core: {:?}", core_expr);
                }
            }
            ReplInput::Decl(_) => {
                println!("Declarations not supported")
            }
        }
        /*
        if let Ok(ri) = result {
            match ri {
                ReplInput::Expr(ast) => {
                    let core_expr = ast.transpile(&sym_env);
                    if args.is_present("core") {
                        println!("Core: {:?}", core_expr);
                    }
                    let node_ptr = core_expr.compile(&mut heap, &nenv);
                    let result_ptr = {
                        let mut machine = TiMachine::new(&mut heap, node_ptr);
                        if args.is_present("step") {
                            println!("Before Step 1");
                            println!("{}", machine);
                            let mut i = 0;
                            while machine.step() {
                                i += 1;
                                println!("After Step {}", i);
                                println!();
                                println!("{}", machine);
                            }
                            i += 1;
                            println!("After Step {}", i);
                            println!();
                            println!("{}", machine);
                            machine.result()
                        } else {
                            machine.run()
                        }
                    };
                    let result = heap.at(result_ptr);
                    println!("{}", result)
                },
                ReplInput::Decl(decl) => {
                    let (binds, child_env) = decl.transpile(&sym_env);
                    // compile the bindings
                    for b in binds {
                        if args.is_present("core") {
                            println!("Core: {:?}", b);
                        }
                        let new_nenv = b.compile(&mut heap, &nenv);
                        let child_nodes = new_nenv.nodes;
                        nenv.extend(child_nodes);
                    }
                    // collect the symbols into our symbol environment
                    // for future compilation
                    let child_symbols = child_env.symbols;
                    sym_env.extend(child_symbols);
                },
                ReplInput::Type(_) => panic!("Cannot handle REPL type input yet!")
            };
        }
        */
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
