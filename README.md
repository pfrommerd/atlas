# Atlas

[![Build Status](https://github.com/atlas-language/atlas-core/actions/workflows/rust.yml/badge.svg)](https://github.com/atlas-language/atlas-core/actions/workflows/rust.yml)


Atlas is a purely-functional programming langauge designed to be embedded in a rust application.

The main purpose of atlas is to enable reproducible, distributed builds. To do this there are several key builtin language features currently planned:
  1) Lazy evaluation
  2) Automatic memoization
  3) Implicit dependencies (for memoization, i.e files are treated as "hidden arguments" to functions and used in the cache)
  4) Binary serializaiton 
      --> allows for distributing precompiled versions of packages that act identically to a from-source build.

We hope to build the following services
 - [ ] Core functional language for arbitrary iterative computation problems
 - [ ] Local code build system
 - [ ] Packaging system/server
 - [ ] Containerization/deployment reusing the build system infrastructure
 - [ ] NixOS-style OS
 - [ ] Distributed builds?

Additional research
 - [ ] Evaluate salsa library for dependency queries
 - [ ] Evaluate serialization formats. Do we want to use existing KV databases such as sled or roll our own using capnp or similar? How would this work in distributed build setting.
 - [ ] Webassembly sandboxing/possibility of in-browser code builds

## Core Language Progress
- [x] Lexer
- [x] AST parser
- [x] Write simple graph reduction interpreter for testing purposes
- [x] Redesign for gmachine/caching
- [x] Lazy Virtual Machine Bytecode
- [ ] Simple Lazy Virtual Machine w/o optimization -- In Progress
- [ ] Simple single-shot build system
- [ ] MVP language features with structured data
- [ ] Automatic memoization/tracing support
- [ ] Binary format and optimizations
- [ ] Better error reporting, debugging information in bytecode
- [ ] Importing/module system (will leverage tracing/memoization)
- [ ] Garbage collection (less important since we aren't running continuously)
- [ ] Package system w/github support
- [ ] Dummy "toolchain"
- [ ] Distributed building/execution/cache
