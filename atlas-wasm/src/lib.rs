//! A deterministic, resource-limited Wasmtime primitive for Atlas.

use std::borrow::Cow;

use atlas_core::extension::{Extensions, Handle, PrimReduce};
use atlas_core::vm::exec::{ExecPolicy, Executor};
use atlas_core::vm::heap::Boxed;
use atlas_core::vm::term::{PrimId, Term};
use wasmtime::{Config, Engine, Instance, Module, Store, StoreLimits, StoreLimitsBuilder};

const WASM_ID: u64 = 0;

/// Limits applied to every WebAssembly invocation.
///
/// Memory growth is allowed up to [`memory_size`](Self::memory_size). Its
/// success can depend on host allocation availability, so callers requiring
/// cross-host determinism must use modules that do not grow memory or tables.
#[derive(Debug, Clone, Copy)]
pub struct WasmConfig {
    pub fuel: u64,
    pub memory_size: usize,
}

impl Default for WasmConfig {
    fn default() -> Self {
        WasmConfig {
            fuel: 10_000_000,
            memory_size: 64 * 1024 * 1024,
        }
    }
}

struct StoreState {
    limits: StoreLimits,
}

/// Atlas host primitives backed by Wasmtime.
///
/// `%wasm module input` compiles `module` as WebAssembly and invokes its
/// exported `run` function. The function must be `(i64) -> i64` for an Atlas
/// integer input or `(f64) -> f64` for an Atlas float input.
pub struct WasmExtensions {
    engine: Engine,
    config: WasmConfig,
}

impl WasmExtensions {
    pub fn new(config: WasmConfig) -> Result<Self, String> {
        let mut engine_config = Config::new();
        engine_config.consume_fuel(true);
        engine_config.cranelift_nan_canonicalization(true);
        engine_config.wasm_relaxed_simd(false);
        let engine = Engine::new(&engine_config).map_err(|error| error.to_string())?;
        Ok(WasmExtensions { engine, config })
    }

    fn store(&self) -> Result<Store<StoreState>, String> {
        let limits = StoreLimitsBuilder::new()
            .memory_size(self.config.memory_size)
            .build();
        let mut store = Store::new(&self.engine, StoreState { limits });
        store.limiter(|state| &mut state.limits);
        store
            .set_fuel(self.config.fuel)
            .map_err(|error| error.to_string())?;
        Ok(store)
    }
}

impl Default for WasmExtensions {
    fn default() -> Self {
        Self::new(WasmConfig::default()).expect("valid default Wasmtime configuration")
    }
}

impl Extensions for WasmExtensions {
    fn resolve(&self, name: &str) -> Option<PrimId> {
        (name == "wasm").then(|| PrimId::new(WASM_ID))
    }

    fn arity(&self, id: PrimId) -> usize {
        assert_eq!(id.get(), WASM_ID, "unknown atlas-wasm primitive");
        2
    }

    fn name(&self, id: PrimId) -> Option<Cow<'_, str>> {
        (id.get() == WASM_ID).then_some(Cow::Borrowed("wasm"))
    }

    fn apply<'a, 'e, 'h, P: ExecPolicy, X: Extensions>(
        &'a self,
        exec: &'a Executor<'e, 'h, P, X>,
        id: PrimId,
        args: Vec<Handle<'h>>,
    ) -> PrimReduce<'a, 'h> {
        Box::pin(async move {
            if id.get() != WASM_ID {
                return Err("unknown atlas-wasm primitive".to_string());
            }
            let mut args = args.into_iter();
            let module = exec
                .whnf_at(args.next().expect("wasm module argument"))
                .await;
            let input = exec
                .whnf_at(args.next().expect("wasm input argument"))
                .await;
            let bytes = match &*module.view() {
                Term::Box(value) => match exec.heap.value_get(value) {
                    Boxed::Bytes(bytes) => bytes.clone(),
                    _ => return Err("%wasm expects its first argument to be Bytes".to_string()),
                },
                _ => return Err("%wasm expects its first argument to be Bytes".to_string()),
            };
            let module = Module::new(&self.engine, bytes)
                .map_err(|error| format!("invalid WebAssembly module: {error}"))?;
            if module.imports().next().is_some() {
                return Err("%wasm modules must not import host functionality".to_string());
            }
            let mut store = self.store()?;
            let instance = Instance::new(&mut store, &module, &[])
                .map_err(|error| format!("failed to instantiate WebAssembly module: {error}"))?;
            let result = match &*input.view() {
                Term::Int(value) => instance
                    .get_typed_func::<i64, i64>(&mut store, "run")
                    .map_err(|error| format!("%wasm expects run: (i64) -> i64: {error}"))?
                    .call(&mut store, *value)
                    .map(Term::Int),
                Term::Float(value) => instance
                    .get_typed_func::<f64, f64>(&mut store, "run")
                    .map_err(|error| format!("%wasm expects run: (f64) -> f64: {error}"))?
                    .call(&mut store, value.into_inner())
                    .map(|value| Term::Float(value.into())),
                _ => return Err("%wasm input must be an Int or Float".to_string()),
            }
            .map_err(|error| format!("WebAssembly run trapped: {error}"))?;
            Ok(Handle::new(exec.heap.alloc(result), exec.heap))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use atlas_core::core::expr::{Expr, Value};
    use atlas_core::vm::exec::{Executor, UnlimitedBudget};
    use atlas_core::vm::heap::Heap;
    use atlas_core::vm::printer::Printer;

    fn wasm(source: &str) -> Vec<u8> {
        wat::parse_str(source).unwrap()
    }

    fn run(extension: &WasmExtensions, module: Vec<u8>, input: Value) -> Result<String, String> {
        let expr = Expr::App {
            func: Box::new(Expr::App {
                func: Box::new(Expr::Pri("wasm".to_string())),
                arg: Box::new(Expr::Value(Value::Bytes(module))),
            }),
            arg: Box::new(Expr::Value(input)),
        };
        let heap = Heap::new();
        heap.with(|h| {
            let root = h.lower(&expr, &|name| extension.resolve(name), &mut |_| None)?;
            let exec = Executor::with_extensions(h, UnlimitedBudget, extension);
            let runtime = tokio::runtime::Builder::new_current_thread()
                .build()
                .unwrap();
            let result = runtime.block_on(exec.normalize_at(root));
            if let Some(error) = exec.take_extension_error() {
                return Err(error);
            }
            Ok(Printer::new(h).pretty(&result).to_string())
        })
    }

    #[test]
    fn calls_integer_run_function() {
        let module = wasm(
            "(module (func (export \"run\") (param i64) (result i64) local.get 0 i64.const 1 i64.add))",
        );
        assert_eq!(
            run(&WasmExtensions::default(), module, Value::Int(41)).unwrap(),
            "42"
        );
    }

    #[test]
    fn calls_float_run_function() {
        let module = wasm(
            "(module (func (export \"run\") (param f64) (result f64) local.get 0 f64.const 0.5 f64.add))",
        );
        assert_eq!(
            run(&WasmExtensions::default(), module, Value::Float(1.5.into())).unwrap(),
            "2.0"
        );
    }

    #[test]
    fn rejects_imports() {
        let module = wasm(
            "(module (import \"env\" \"x\" (func)) (func (export \"run\") (param i64) (result i64) local.get 0))",
        );
        let error = run(&WasmExtensions::default(), module, Value::Int(1)).unwrap_err();
        assert!(error.contains("must not import"));
    }

    #[test]
    fn reports_invalid_modules_and_traps() {
        let extension = WasmExtensions::default();
        assert!(
            run(&extension, vec![0, 1, 2], Value::Int(1))
                .unwrap_err()
                .contains("invalid WebAssembly module")
        );
        let trap = wasm("(module (func (export \"run\") (param i64) (result i64) unreachable))");
        assert!(
            run(&extension, trap, Value::Int(1))
                .unwrap_err()
                .contains("trapped")
        );
        let missing_run =
            wasm("(module (func (export \"other\") (param i64) (result i64) local.get 0))");
        assert!(
            run(&extension, missing_run, Value::Int(1))
                .unwrap_err()
                .contains("expects run")
        );
    }

    #[test]
    fn enforces_memory_limit() {
        let module = wasm(
            "(module (memory 1) (func (export \"run\") (param i64) (result i64) local.get 0))",
        );
        let extension = WasmExtensions::new(WasmConfig {
            fuel: 10_000_000,
            memory_size: 0,
        })
        .unwrap();
        assert!(
            run(&extension, module, Value::Int(1))
                .unwrap_err()
                .contains("instantiate")
        );
    }
}
