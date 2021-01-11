extern crate clap;

use clap::{App, Arg, ArgMatches, SubCommand};

use atlas::parse::lexer::Lexer;
use atlas::grammar;
use atlas::core::lang::{Expr, Id, Symbol, SymbolEnv};
use atlas::parse::ast::{ReplInput, Module, Expr as AstExpr, Span};
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

    let f_ptr = heap.add(Node::Bad);
    let f_body = heap.add(Node::ArgRef(1, f_ptr)); // get the second arg
    heap.set(f_ptr, Node::Combinator(2, f_body));

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
            let core_expr = match ri {
                ReplInput::Expr(ast) => Expr::transpile_expr(&sym_env, &ast),
                ReplInput::Decl(decl) => {
                    let m = AstExpr::Module(Span::new(0, 0), 
                                    Module::new(vec![decl]));
                    Expr::transpile_expr(&sym_env, &m)
                },
                ReplInput::Type(_) => Expr::Bad
            };
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
