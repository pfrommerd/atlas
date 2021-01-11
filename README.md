# Atlas

What is atlas?

Atlas is a purely-functional programming langauge designed to be embedded in a rust application.

The main purpose of atlas is to enable reproducible, distributed builds. To do this there are several key builtin language features currently planned:
  1) Lazy evaluation
  2) Automatic memoization
  3) Implicit dependencies (for memoization, i.e files are treated as "hidden arguments" to functions and used in the cache)
  4) Binary serializaiton 
      --> allows for distributing precompiled versions of packages that act identically to a from-source build.

In terms of

In style it is similar to ocaml and gluon

## Roadmap
- [x] Write lexer
- [x] Write AST --> untyped lambda expression compiler
- [ ] Write untyped lambda exression -> typed lambda expression compiler
- [x] Write simple graph reduction interpreter for lambda expressions
- [ ] Basic build system that rebuilds everything every time invoked
- [ ] Smart error handling during execution to allow maximum number of files to be compiled
- [ ] Implicit dependencies/arguments and automatic memoization
- [ ] Modules and import/export system
- [ ] Package system, well defined build workspace vs package separation
- [ ] Binary serialization
- [ ] Distributed building/execution
