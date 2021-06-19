extern crate clap;

use clap::{App, Arg, ArgMatches, SubCommand};

use atlas::parse::lexer::Lexer;
use atlas::grammar;
use atlas::core::lang::{SymbolEnv};
use atlas::parse::ast::{ReplInput, Expr as AstExpr};
use atlas::interp::tim::TiMachine;
use atlas::interp::node::{Heap, NodeEnv};
use atlas::interp::compile::{Compile, CompileEnv};

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
    let mut heap = Heap::new();
    let nenv = NodeEnv::default(&mut heap);
    let senv = SymbolEnv::default();
    let ast = AstExpr::Module(parsed);
    let expr = ast.transpile(&senv);
    println!("Core: {:?}", expr);
    let nptr = expr.compile(&mut heap, &nenv);
    println!("Root {}", nptr);
    println!("{}", heap);
    let mut machine = TiMachine::new(&mut heap, nptr);
    machine.run();
    let result = heap.at(nptr);
    println!("{}", result);
    /* code for extracting only a single entry from the module (untested)
    // assign the module to a symbol for easy evaluation
    // extract the entry we want
    let s = Symbol::new(String::from("mod"), 0);
    let mut menv = NodeEnv::new();
    menv.set(s.clone(), nptr);
    // construct a simple core expression which extracts
    // the main function from the module type
    // and let us evaluate main
    let cast = Expr::Cast{expr: Atom::Id(s), new_type: 
        Atom::Type(Type::Record(vec![(String::from("main"), Type::Any.expr())]))};
    let val = Expr::Unpack(cast.as_atom(), 0);
    let result_ptr = val.compile(&mut heap, &menv);
    let mut machine = TiMachine::new(&mut heap, result_ptr);
    machine.run();
    let result = heap.at(result_ptr);
    println!("{}", result)
    */
}

fn interactive(args: &ArgMatches) {
    use std::io::{stdin, stdout, Write};

    let mut heap = Heap::new();
    // create a default node environment
    let mut nenv = NodeEnv::default(&mut heap);
    let mut sym_env = SymbolEnv::default();

    loop {
        print!(">> ");
        let _ = stdout().flush();

        let mut input = String::new();
        let res = stdin().read_line(&mut input);
        match res {
            Err(_) => break,
            Ok(len) => if len == 0 { break }
        }

        if input.trim().len() == 0 {
            continue
        }

        let lexer = Lexer::new(&input);
        let parser = grammar::ReplInputParser::new();
        let result = parser.parse(lexer);

        if args.is_present("parse") {
            println!("Parse: {:?}", result);
        }
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
    }
}

fn main() {
    let matches = App::new("Atlas Build System")
                    .version("pre-alpha")
                    .author("Daniel Pfrommer <dan.pfrommer@gmail.com>")
                    .about("A cutting-edge build system")
                    .subcommand(SubCommand::with_name("run")
                        .arg(Arg::with_name("INPUT")
                            .required(true)))
                    .subcommand(SubCommand::with_name("interactive")
                        .arg(Arg::with_name("parse")
                              .short("p")
                              .help("Print parse tree"))
                        .arg(Arg::with_name("step")
                              .short("s")
                              .help("Step through evals"))
                        .arg(Arg::with_name("core")
                              .short("c")
                              .help("Core Expression"))
                        .about("interactive REPL input")).get_matches();

    if let Some(args) = matches.subcommand_matches("interactive") {
        interactive(args);
    } else if let Some(args) = matches.subcommand_matches("run") {
        run(args);
    } else {
        println!("Taking a nap....no command specified");
    }
}
