//! Small filesystem-reading primitives for hosts that intentionally opt in.

use std::borrow::Cow;
use std::sync::Arc;

use super::{Extensions, Handle, PrimReduce};
use crate::vm::exec::{ExecPolicy, Executor};
use crate::vm::heap::Boxed;
use crate::vm::term::{PrimId, Term};

/// Filesystem-reading primitives: `%read_binary path` and `%read_text path`.
#[derive(Debug, Clone, Copy, Default)]
pub struct SimpleIO;

impl Extensions for SimpleIO {
    fn resolve(&self, name: &str) -> Option<PrimId> {
        match name {
            "read_binary" => Some(PrimId::new(0)),
            "read_text" => Some(PrimId::new(1)),
            _ => None,
        }
    }

    fn arity(&self, id: PrimId) -> usize {
        match id.get() {
            0 | 1 => 1,
            _ => unreachable!("unknown SimpleIO primitive"),
        }
    }

    fn name(&self, id: PrimId) -> Option<Cow<'_, str>> {
        Some(Cow::Borrowed(match id.get() {
            0 => "read_binary",
            1 => "read_text",
            _ => return None,
        }))
    }

    fn apply<'a, 'e, 'h, P: ExecPolicy, X: Extensions>(
        &'a self,
        exec: &'a Executor<'e, 'h, P, X>,
        id: PrimId,
        args: Vec<Handle<'h>>,
    ) -> PrimReduce<'a, 'h> {
        Box::pin(async move {
            let path = exec
                .whnf_at(args.into_iter().next().expect("path argument"))
                .await;
            let path = match &*path.view() {
                Term::Box(value) => match exec.heap.value_get(value) {
                    Boxed::Str(path) => path.clone(),
                    _ => return Err(format!("%{} expects a String path", self.name(id).unwrap())),
                },
                _ => return Err(format!("%{} expects a String path", self.name(id).unwrap())),
            };
            let bytes = std::fs::read(path.as_ref())
                .map_err(|error| format!("cannot read {path:?}: {error}"))?;
            let value = match id.get() {
                0 => Boxed::Bytes(Arc::from(bytes)),
                1 => {
                    Boxed::Str(Arc::from(String::from_utf8(bytes).map_err(|error| {
                        format!("{path:?} is not valid UTF-8: {error}")
                    })?))
                }
                _ => return Err("unknown SimpleIO primitive".to_string()),
            };
            Ok(Handle::new(
                exec.heap.alloc(Term::Box(exec.heap.value(value))),
                exec.heap,
            ))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("atlas-simple-io-{}-{name}", std::process::id()))
    }

    #[test]
    fn reads_text_and_binary_files() {
        let text = path("text");
        let binary = path("binary");
        std::fs::write(&text, "hello").unwrap();
        std::fs::write(&binary, [0, 1, 255]).unwrap();
        let extension = SimpleIO;
        assert_eq!(
            crate::vm::run_with(&format!(r#"%read_text {:?}"#, text), &extension).unwrap(),
            "\"hello\""
        );
        assert_eq!(
            crate::vm::run_with(&format!(r#"%read_binary {:?}"#, binary), &extension).unwrap(),
            "[0, 1, 255]"
        );
        std::fs::remove_file(text).unwrap();
        std::fs::remove_file(binary).unwrap();
    }

    #[test]
    fn reports_read_and_utf8_errors() {
        let missing = path("missing");
        let invalid = path("invalid");
        std::fs::write(&invalid, [255]).unwrap();
        let extension = SimpleIO;
        assert!(
            crate::vm::run_with(&format!(r#"%read_text {:?}"#, missing), &extension)
                .unwrap_err()
                .contains("cannot read")
        );
        assert!(
            crate::vm::run_with(&format!(r#"%read_text {:?}"#, invalid), &extension)
                .unwrap_err()
                .contains("not valid UTF-8")
        );
        std::fs::remove_file(invalid).unwrap();
    }
}
