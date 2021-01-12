extern crate clap;

use clap::{App, Arg, ArgMatches, SubCommand};

use atlas::parse::lexer::Lexer;
use atlas::grammar;
use atlas::core::lang::{Id, Symbol, SymbolEnv};
use atlas::parse::ast::{ReplInput};
use atlas::interp::tim::TiMachine;
use atlas::interp::node::{Node, Heap, Primitive, NodeEnv};

fn interactive(args: &ArgMatches) {
    use std::io::{stdin, stdout, Write};

    let mut heap = Heap::new();
    // create a default node environment
    let mut nenv = NodeEnv::default(&mut heap);
    let mut sym_env = SymbolEnv::default();

    // put examples a and f
    sym_env.add(Symbol::new(Id::new(String::from("a"), 0)));
    sym_env.add(Symbol::new(Id::new(String::from("f"), 0)));

    nenv.set(Id::new(String::from("a"), 0), 
             heap.add(Node::Prim(Primitive::Int(123))));

    let f_body = heap.add(Node::Bad);
    heap.set(f_body, Node::ArgRef(1, f_body)); // get the second arg
    let f_ptr = heap.add(Node::Combinator(2, f_body));

    nenv.set(Id::new(String::from("f"), 0), f_ptr);


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
                    let node_ptr = Node::compile(&mut heap, &core_expr, &nenv);
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
                        let new_nenv = Node::compile_bind(&mut heap, &b, &nenv);
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
    } else {
        println!("Taking a nap....no command specified");
    }
}
