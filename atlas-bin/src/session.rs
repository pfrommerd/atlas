//! One REPL session: a branded heap, its async runtime, and the locals in
//! scope. Lifted from the old `core-repl` example, with all printing replaced
//! by [`SubmitResult`] so the TUI (and tests) can render outputs themselves.

use std::collections::HashMap;

use atlas_core::core::ast::{desugar_open, Binding, Node};
use atlas_core::core::expr::Expr;
use atlas_core::core::parse::{parse_repl, ReplInput};
use atlas_core::extension::{CombinedExtensions, Extensions};
use atlas_core::vm::heap::{HeapScope, TermPtr};
use atlas_core::vm::printer::Printer;
use atlas_io::IoExtensions;
use atlas_wasm::WasmExtensions;

const PRELUDE: &str = include_str!("prelude.atc");

pub type ReplExtensions = CombinedExtensions<IoExtensions, WasmExtensions>;

/// Which language the REPL interprets a line as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LangMode {
    /// The interaction-calculus core language: parse, lower, and evaluate.
    Core,
    /// The main surface atlas language: parse, lower to the core IR, and
    /// evaluate.
    Atlas,
    /// Reserved for agent interactions. It currently accepts input without
    /// evaluating it.
    Agent,
}

impl LangMode {
    pub fn label(self) -> &'static str {
        match self {
            LangMode::Core => "core",
            LangMode::Atlas => "atlas",
            LangMode::Agent => "agent",
        }
    }

    pub fn next(self) -> Self {
        match self {
            LangMode::Core => LangMode::Atlas,
            LangMode::Atlas => LangMode::Agent,
            LangMode::Agent => LangMode::Core,
        }
    }
}

/// Whether a REPL local is consumed on use or duplicated on every use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalKind {
    /// `x = …`: affine; the term is taken (and the binding removed) on first use.
    Affine,
    /// `&x = …`: auto-dup; each use splices a fresh dup, keeping the binding.
    AutoDup,
}

impl LocalKind {
    pub fn label(self) -> &'static str {
        match self {
            LocalKind::Affine => "affine",
            LocalKind::AutoDup => "auto-dup",
        }
    }
}

/// A REPL local binding: its (lazy) value term and how it is consumed.
struct Local<'h> {
    ptr: TermPtr<'h>,
    kind: LocalKind,
}

/// The locals in scope, keyed by name. The stored [`TermPtr`] is a live node in
/// the session heap.
struct Locals<'h> {
    map: HashMap<String, Local<'h>>,
}

impl<'h> Locals<'h> {
    fn new() -> Self {
        Locals {
            map: HashMap::new(),
        }
    }

    /// Bind (or rebind) `name` to `ptr`. A redefinition simply overwrites.
    fn bind(&mut self, name: String, kind: LocalKind, ptr: TermPtr<'h>) {
        self.map.insert(name, Local { ptr, kind });
    }

    /// Resolve a use site. Affine locals are taken and removed; auto-dup locals
    /// are duplicated via [`HeapScope::dup_use`], keeping the second ("keep")
    /// projection as the binding's new value so it survives for the next use.
    fn use_name(&mut self, name: &str, h: &'h HeapScope<'h>) -> Option<TermPtr<'h>> {
        let kind = self.map.get(name)?.kind;
        match kind {
            LocalKind::Affine => Some(self.map.remove(name).unwrap().ptr),
            LocalKind::AutoDup => {
                let ptr = self.map.remove(name).unwrap().ptr;
                let (use_node, keep_node) = h.dup_use(ptr);
                self.bind(name.to_string(), LocalKind::AutoDup, keep_node);
                Some(use_node)
            }
        }
    }
}

/// The outcome of submitting one line to the session.
pub enum SubmitResult<'h> {
    /// A core expression was lowered; the caller should start evaluating it.
    /// `output` holds stage dumps (AST, desugared, transpiled) when `show_ast`
    /// is enabled.
    StartEval {
        root: TermPtr<'h>,
        output: Vec<String>,
    },
    /// Plain output blocks (stage dumps when `show_ast` is enabled).
    Output(Vec<String>),
    /// A failure. `output` holds the stage dumps produced *before* the failing
    /// step (when `show_ast` is enabled), so a desugar/lowering error still
    /// shows the stages that succeeded.
    Error {
        message: String,
        output: Vec<String>,
    },
}

pub struct Session<'h> {
    pub h: &'h HeapScope<'h>,
    pub runtime: tokio::runtime::Runtime,
    pub extensions: ReplExtensions,
    locals: Locals<'h>,
    /// Reduction budget: the maximum number of interactions per evaluation.
    pub budget: u64,
    /// Strong normalization (reduce under binders / into sub-terms) when set;
    /// weak head normal form otherwise.
    pub strong: bool,
    /// Dump the AST of each submitted line (both languages; `/show ast`).
    pub show_ast: bool,
    /// Atlas enum variants in scope: variant name -> the local binding of the
    /// enum type it constructs (fed to the atlas lowering as its ctor map).
    atlas_ctors: HashMap<String, String>,
}

impl<'h> Session<'h> {
    pub fn new(h: &'h HeapScope<'h>, budget: u64, strong: bool) -> Self {
        // Deterministic single-threaded runtime (no need for the multi-threaded
        // scheduler here); reduction is driven via `block_on`.
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("failed to build tokio runtime");
        Session {
            h,
            runtime,
            extensions: ReplExtensions::new(IoExtensions, WasmExtensions::default()),
            locals: Locals::new(),
            budget,
            strong,
            show_ast: false,
            atlas_ctors: HashMap::new(),
        }
    }

    /// Parse one line in `mode`: lower a core expression for evaluation, apply
    /// core `lhs = rhs;` bindings, or (atlas mode) parse the surface language
    /// (dumping ASTs along the way when `show_ast` is set).
    pub fn submit(&mut self, mode: LangMode, line: &str) -> SubmitResult<'h> {
        match mode {
            LangMode::Core => self.submit_core(line),
            LangMode::Atlas => self.submit_atlas(line),
            LangMode::Agent => SubmitResult::Output(Vec::new()),
        }
    }

    pub fn load_prelude(&mut self) -> SubmitResult<'h> {
        self.submit_core(PRELUDE)
    }

    fn submit_core(&mut self, line: &str) -> SubmitResult<'h> {
        match parse_repl(line) {
            Ok(ReplInput::Expr(node)) => {
                let mut output = Vec::new();
                match self.lower_input(&node, &mut output) {
                    Ok(root) => SubmitResult::StartEval { root, output },
                    Err(message) => SubmitResult::Error { message, output },
                }
            }
            Ok(ReplInput::Decl(bindings)) => self.bind_decl(bindings),
            Err(message) => SubmitResult::Error {
                message,
                output: Vec::new(),
            },
        }
    }

    /// Parse one atlas line: lower an expression to the core IR for evaluation,
    /// or bind a declaration (`let` / `fn` / `enum`) as a session local.
    fn submit_atlas(&mut self, line: &str) -> SubmitResult<'h> {
        match atlas_lang::parser::parse_repl(line) {
            Ok(atlas_lang::ast::ReplInput::Expr(e)) => {
                let mut output = Vec::new();
                if self.show_ast {
                    output.push(format!("{e:#?}"));
                }
                let lowered = atlas_lang::lower::lower_expr_open(&e, &self.atlas_ctors)
                    .and_then(|expr| self.lower_core(&expr, &mut output));
                match lowered {
                    Ok(root) => SubmitResult::StartEval { root, output },
                    Err(message) => SubmitResult::Error { message, output },
                }
            }
            Ok(atlas_lang::ast::ReplInput::Declaration(decl)) => {
                let mut output = Vec::new();
                if self.show_ast {
                    output.push(format!("{decl:#?}"));
                }
                match self.bind_atlas_decl(&decl, &mut output) {
                    Ok(()) => SubmitResult::Output(output),
                    Err(message) => SubmitResult::Error { message, output },
                }
            }
            Err(message) => SubmitResult::Error {
                message,
                output: Vec::new(),
            },
        }
    }

    /// Lower one atlas declaration and store it as a local (lazily, like core
    /// bindings). Atlas has no affine/auto-dup annotation, so every binding is
    /// auto-dup (usable any number of times). Enum declarations additionally
    /// register their variant names so later lines construct the bound type.
    fn bind_atlas_decl(
        &mut self,
        decl: &atlas_lang::ast::Declaration,
        output: &mut Vec<String>,
    ) -> Result<(), String> {
        let lowered = match atlas_lang::lower::lower_decl_open(decl, &self.atlas_ctors)? {
            Some(lowered) => lowered,
            // a `let _ = ..;`: the value is dropped
            None => return Ok(()),
        };
        let ptr = self.lower_core(&lowered.expr, output)?;
        self.locals
            .bind(lowered.name.clone(), LocalKind::AutoDup, ptr);
        for variant in lowered.variants {
            self.atlas_ctors.insert(variant, lowered.name.clone());
        }
        Ok(())
    }

    /// Source a file into the session, dispatching on its extension: `.atc` is
    /// core input (a run of `lhs = rhs;` bindings and/or a trailing expression,
    /// exactly like a REPL line), `.at` is an atlas module (parse only, since
    /// atlas evaluation is not implemented yet).
    pub fn source_file(&mut self, path: &std::path::Path) -> SubmitResult<'h> {
        let src = match std::fs::read_to_string(path) {
            Ok(src) => src,
            Err(e) => {
                return SubmitResult::Error {
                    message: format!("cannot read {}: {e}", path.display()),
                    output: Vec::new(),
                };
            }
        };
        match path.extension().and_then(|e| e.to_str()) {
            Some("atc") => self.submit_core(&src),
            Some("at") => match atlas_lang::parser::parse_module(&src) {
                Ok(module) => {
                    let mut output = Vec::new();
                    if self.show_ast {
                        output.push(format!("{module:#?}"));
                    }
                    output.push(format!(
                        "parsed {} declaration{} (atlas evaluation not yet implemented)",
                        module.decls.len(),
                        if module.decls.len() == 1 { "" } else { "s" },
                    ));
                    SubmitResult::Output(output)
                }
                Err(message) => SubmitResult::Error {
                    message,
                    output: Vec::new(),
                },
            },
            _ => SubmitResult::Error {
                message: format!(
                    "{}: unknown extension (expected .atc for core or .at for atlas)",
                    path.display()
                ),
                output: Vec::new(),
            },
        }
    }

    /// Desugar and lower a surface node into the session heap, resolving any
    /// free names against the locals in scope. When `show_ast` is set, each
    /// stage is dumped into `output` as it completes — the parsed node, the
    /// desugared core expression, and the transpiled heap term — so a failing
    /// step still leaves the earlier stages visible.
    fn lower_input(
        &mut self,
        node: &Node,
        output: &mut Vec<String>,
    ) -> Result<TermPtr<'h>, String> {
        if self.show_ast {
            output.push(format!("{node:#?}"));
        }
        let expr = desugar_open(node)?;
        self.lower_core(&expr, output)
    }

    /// Lower a desugared core expression into the session heap, resolving free
    /// names against the locals in scope (the shared back half of both the core
    /// and atlas pipelines). When `show_ast` is set, the desugared expression
    /// and the transpiled heap term are dumped into `output`.
    fn lower_core(&mut self, expr: &Expr, output: &mut Vec<String>) -> Result<TermPtr<'h>, String> {
        if self.show_ast {
            output.push(format!("desugared:\n{expr}"));
        }
        let prim = |name: &str| self.extensions.resolve(name);
        // Copy out the (Copy) heap reference so the closure can borrow `locals`.
        let h = self.h;
        let locals = &mut self.locals;
        let root = h.lower(expr, &prim, &mut |n| locals.use_name(n, h))?;
        if self.show_ast {
            let printer = Printer::new(h);
            output.push(format!("transpiled:\n{}", printer.pretty(&root)));
        }
        Ok(root)
    }

    /// Lower each `lhs = rhs;` and store it as a local (lazily — the value is
    /// not reduced until used). Bindings are silent apart from AST dumps.
    fn bind_decl(&mut self, bindings: Vec<(Binding, Node)>) -> SubmitResult<'h> {
        let mut output = Vec::new();
        for (binding, value) in bindings {
            let (name, kind) = match binding {
                Binding::Var { name, auto_dup } => (
                    name.to_string(),
                    if auto_dup {
                        LocalKind::AutoDup
                    } else {
                        LocalKind::Affine
                    },
                ),
                Binding::Hole => {
                    return SubmitResult::Error {
                        message: "`_` bindings are not supported in the REPL".to_string(),
                        output,
                    };
                }
                Binding::Dup { .. } => {
                    return SubmitResult::Error {
                        message: "explicit dup bindings (&L{..}) are not supported in the REPL"
                            .to_string(),
                        output,
                    };
                }
            };
            match self.lower_input(&value, &mut output) {
                Ok(ptr) => self.locals.bind(name, kind, ptr),
                Err(message) => return SubmitResult::Error { message, output },
            }
        }
        SubmitResult::Output(output)
    }

    /// The locals in scope (sorted by name), for `/locals` and as heap-explorer
    /// roots.
    pub fn locals(&self) -> Vec<(&str, LocalKind, &TermPtr<'h>)> {
        let mut entries: Vec<_> = self
            .locals
            .map
            .iter()
            .map(|(name, local)| (name.as_str(), local.kind, &local.ptr))
            .collect();
        entries.sort_by_key(|&(name, ..)| name);
        entries
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use atlas_core::vm::heap::Heap;
    use atlas_core::vm::printer::Printer;

    fn eval_to_string(session: &mut Session, line: &str) -> String {
        match session.submit(LangMode::Core, line) {
            SubmitResult::StartEval { root, .. } => {
                let root = crate::eval::run_to_completion(session, root);
                let out = Printer::new(session.h).pretty(&root).to_string();
                crate::eval::erase(session, root);
                out
            }
            SubmitResult::Output(lines) => lines.join("\n"),
            SubmitResult::Error { message, .. } => panic!("submit error: {message}"),
        }
    }

    #[test]
    fn core_expression_evaluates() {
        let heap = Heap::new();
        heap.with(|h| {
            let mut session = Session::new(h, 1_000, false);
            assert_eq!(eval_to_string(&mut session, "(\\x -> x + 1) 2"), "3");
        });
    }

    #[test]
    fn auto_dup_local_survives_uses() {
        let heap = Heap::new();
        heap.with(|h| {
            let mut session = Session::new(h, 10_000, false);
            match session.submit(LangMode::Core, "&f = \\x -> x + 1;") {
                SubmitResult::Output(lines) => assert!(lines.is_empty(), "bindings are silent"),
                _ => panic!("expected binding output"),
            }
            assert_eq!(eval_to_string(&mut session, "f 1"), "2");
            assert_eq!(eval_to_string(&mut session, "f 2"), "3");
            assert_eq!(session.locals().len(), 1);
        });
    }

    #[test]
    fn affine_local_consumed_on_use() {
        let heap = Heap::new();
        heap.with(|h| {
            let mut session = Session::new(h, 1_000, false);
            session.submit(LangMode::Core, "x = 41;");
            assert_eq!(eval_to_string(&mut session, "x + 1"), "42");
            assert!(session.locals().is_empty());
        });
    }

    #[test]
    fn prelude_loads_fib_binding() {
        let heap = Heap::new();
        heap.with(|h| {
            let mut session = Session::new(h, 100_000, false);
            match session.load_prelude() {
                SubmitResult::Output(lines) => assert!(lines.is_empty(), "prelude is silent"),
                SubmitResult::Error { message, .. } => panic!("prelude error: {message}"),
                SubmitResult::StartEval { .. } => panic!("prelude must only contain bindings"),
            }
            assert!(session.locals().iter().any(|(name, ..)| *name == "fib"));
            assert_eq!(eval_to_string(&mut session, "fib 5"), "8");
        });
    }

    #[test]
    fn atlas_mode_silent_unless_show_ast() {
        let heap = Heap::new();
        heap.with(|h| {
            let mut session = Session::new(h, 1_000, false);
            match session.submit(LangMode::Atlas, "let a = 1") {
                SubmitResult::Output(lines) => assert!(lines.is_empty(), "silent by default"),
                SubmitResult::Error { message, .. } => panic!("parse error: {message}"),
                SubmitResult::StartEval { .. } => panic!("atlas mode must not evaluate"),
            }
            session.show_ast = true;
            match session.submit(LangMode::Atlas, "let a = 1") {
                SubmitResult::Output(lines) => {
                    assert!(lines[0].contains("Let"), "AST dump: {}", lines[0])
                }
                SubmitResult::Error { message, .. } => panic!("parse error: {message}"),
                SubmitResult::StartEval { .. } => panic!("atlas mode must not evaluate"),
            }
        });
    }

    #[test]
    fn agent_mode_ignores_input() {
        let heap = Heap::new();
        heap.with(|h| {
            let mut session = Session::new(h, 1_000, false);
            assert!(matches!(
                session.submit(LangMode::Agent, "anything at all"),
                SubmitResult::Output(output) if output.is_empty()
            ));
        });
    }

    #[test]
    fn source_file_dispatches_on_extension() {
        let dir = std::env::temp_dir().join(format!("atlas-source-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let atc = dir.join("defs.atc");
        std::fs::write(&atc, "a = 1 + 1;\n&double = \\&x -> x + x;\n").unwrap();
        let at = dir.join("mod.at");
        std::fs::write(&at, "let a = 1\nlet b = 2\n").unwrap();
        let bogus = dir.join("noext");
        std::fs::write(&bogus, "").unwrap();

        let heap = Heap::new();
        heap.with(|h| {
            let mut session = Session::new(h, 10_000, false);
            // .atc: bindings land in the locals (silently).
            match session.source_file(&atc) {
                SubmitResult::Output(lines) => assert!(lines.is_empty()),
                _ => panic!("expected silent bindings"),
            }
            assert_eq!(session.locals().len(), 2);
            assert_eq!(eval_to_string(&mut session, "double a"), "4");
            // .at: parse-only, reports the declaration count.
            match session.source_file(&at) {
                SubmitResult::Output(lines) => {
                    assert!(lines[0].contains("2 declarations"), "got: {}", lines[0])
                }
                _ => panic!("expected atlas parse output"),
            }
            // unknown extension: an error.
            assert!(matches!(
                session.source_file(&bogus),
                SubmitResult::Error { .. }
            ));
        });
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn repl_combines_io_and_wasm_extensions() {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        use sha2::Digest;

        let wasm = wat::parse_str(
            "(module (func (export \"run\") (param i64) (result i64) local.get 0 i64.const 1 i64.add))",
        )
        .unwrap();
        let hash = format!("sha256-{}", hex::encode(sha2::Sha256::digest(&wasm)));
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0; 1024];
            assert!(stream.read(&mut request).unwrap() > 0);
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                wasm.len()
            )
            .unwrap();
            stream.write_all(&wasm).unwrap();
        });
        let url = format!("http://{address}/module.wasm");
        let heap = Heap::new();
        heap.with(|h| {
            let mut session = Session::new(h, 1_000, false);
            assert_eq!(
                eval_to_string(
                    &mut session,
                    &format!(r#"%wasm (%fetch {url:?} {hash:?}) 41"#)
                ),
                "42"
            );
        });
        server.join().unwrap();
    }

    #[test]
    fn show_ast_dumps_node_and_lowered_expr() {
        let heap = Heap::new();
        heap.with(|h| {
            let mut session = Session::new(h, 1_000, false);
            session.show_ast = true;
            match session.submit(LangMode::Core, "(\\x -> x + 1) 2") {
                SubmitResult::StartEval { root, output } => {
                    assert_eq!(output.len(), 3);
                    assert!(output[0].contains("Lambda"), "Node dump: {}", output[0]);
                    assert!(output[1].starts_with("desugared:"), "Expr: {}", output[1]);
                    assert!(output[2].starts_with("transpiled:"), "term: {}", output[2]);
                    crate::eval::erase(&session, root);
                }
                _ => panic!("expected an evaluation"),
            }
            // Binding values are dumped too.
            match session.submit(LangMode::Core, "y = 1 + 2;") {
                SubmitResult::Output(lines) => assert_eq!(lines.len(), 3),
                _ => panic!("expected binding output"),
            }
        });
    }

    #[test]
    fn show_ast_keeps_stages_before_a_failure() {
        let heap = Heap::new();
        heap.with(|h| {
            let mut session = Session::new(h, 1_000, false);
            session.show_ast = true;
            // Desugar fails (affine `x` used twice): the parsed AST dump — the
            // stage that succeeded — must still come back with the error.
            match session.submit(LangMode::Core, "(\\x -> x + x) 1") {
                SubmitResult::Error { message, output } => {
                    assert!(message.contains("affine variable"), "got: {message}");
                    assert_eq!(output.len(), 1);
                    assert!(output[0].contains("Lambda"), "Node dump: {}", output[0]);
                }
                _ => panic!("expected a desugar error"),
            }
        });
    }
}
