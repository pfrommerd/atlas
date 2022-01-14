# Atlas

[![Build Status](https://github.com/atlas-language/atlas-core/actions/workflows/rust.yml/badge.svg)](https://github.com/atlas-language/atlas-core/actions/workflows/rust.yml)


Atlas is a purely-functional programming langauge designed to be embedded in a rust application.

The main purpose of atlas is to enable reproducible, distributed builds. To do this there are several key builtin language features currently planned:
  1) Lazy evaluation
  2) Automatic memoization
  3) Implicit dependencies (for memoization, i.e files are treated as "hidden arguments" to functions and used in the cache)
  4) Binary serializaiton 
      --> allows for distributing precompiled versions of packages that act identically to a from-source build.

Atlas differs from prior work in the same space (such as NixOS) through its innovative builtin automatic-memoization, infinitely-superscaling virtual machine design for maximum build parallelization, and dynamic hot reloading facilities.

We hope to eventually build the following services ontop of the core language
 - Local code build system
 - Packaging system/remote server with source/binary builds
 - Containerization/deployment reusing the build system infrastructure

In addition, there are several application domains which Atlas could support, but we do not currently plan to implement userspace packages for: 
 - NixOS-style OS definitions
 - Distributed builds
 - Dataset and model management toolset for machine learning
 - Webassembly sandboxing/possibility of in-browser code builds

## Core Language Progress
- [x] Lexer
- [x] AST parser
- [x] Intermediate operation graph
- [x] Lazy infinitely-superscalar virtual machine
- [ ] Simple single-shot build system -- In Progress
- [ ] MVP language features with structured data
- [ ] Automatic memoization/tracing support
- [ ] Binary format and optimizations
- [ ] Better error reporting, debugging information in bytecode
- [ ] Importing/module system (will leverage tracing/memoization)
- [ ] Garbage collection (less important since we aren't running continuously)
- [ ] Package system w/github support
- [ ] Dummy "toolchain"
- [ ] Distributed building/execution/cache
