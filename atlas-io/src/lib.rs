//! Deterministic network access primitives for Atlas.

use std::borrow::Cow;
use std::sync::Arc;

use atlas_core::extension::{Extensions, Handle, PrimReduce};
use atlas_core::vm::exec::{ExecPolicy, Executor};
use atlas_core::vm::heap::Boxed;
use atlas_core::vm::term::{PrimId, Term};
use sha2::Digest;

const FETCH_ID: u64 = 0;

/// Atlas network primitives.
///
/// `%fetch url hash` downloads `url`, verifies the response body against a
/// tagged hexadecimal digest such as `sha256-...` or `md5-...`, and returns
/// the body as `Bytes`.
#[derive(Debug, Clone, Copy, Default)]
pub struct IoExtensions;

#[derive(Clone, Copy)]
enum HashAlgorithm {
    Sha256,
    Md5,
}

impl HashAlgorithm {
    fn parse(hash: &str) -> Result<(Self, Vec<u8>), String> {
        let (algorithm, encoded) = hash
            .split_once('-')
            .ok_or_else(|| "%fetch hash must have the form <algorithm>-<hex digest>".to_string())?;
        let algorithm = match algorithm {
            "sha256" => HashAlgorithm::Sha256,
            "md5" => HashAlgorithm::Md5,
            _ => {
                return Err(format!(
                    "%fetch does not support hash algorithm {algorithm:?}"
                ));
            }
        };
        let expected = hex::decode(encoded)
            .map_err(|error| format!("%fetch hash is not valid hexadecimal: {error}"))?;
        let expected_len = match algorithm {
            HashAlgorithm::Sha256 => 32,
            HashAlgorithm::Md5 => 16,
        };
        if expected.len() != expected_len {
            return Err(format!(
                "%fetch hash has {} bytes, but {algorithm} requires {expected_len}",
                expected.len(),
                algorithm = match algorithm {
                    HashAlgorithm::Sha256 => "sha256",
                    HashAlgorithm::Md5 => "md5",
                }
            ));
        }
        Ok((algorithm, expected))
    }

    fn digest(&self, bytes: &[u8]) -> Vec<u8> {
        match self {
            HashAlgorithm::Sha256 => sha2::Sha256::digest(bytes).to_vec(),
            HashAlgorithm::Md5 => md5::Md5::digest(bytes).to_vec(),
        }
    }
}

impl Extensions for IoExtensions {
    fn resolve(&self, name: &str) -> Option<PrimId> {
        (name == "fetch").then(|| PrimId::new(FETCH_ID))
    }

    fn arity(&self, id: PrimId) -> usize {
        assert_eq!(id.get(), FETCH_ID, "unknown atlas-io primitive");
        2
    }

    fn name(&self, id: PrimId) -> Option<Cow<'_, str>> {
        (id.get() == FETCH_ID).then_some(Cow::Borrowed("fetch"))
    }

    fn apply<'a, 'e, 'h, P: ExecPolicy, X: Extensions>(
        &'a self,
        exec: &'a Executor<'e, 'h, P, X>,
        id: PrimId,
        args: Vec<Handle<'h>>,
    ) -> PrimReduce<'a, 'h> {
        Box::pin(async move {
            if id.get() != FETCH_ID {
                return Err("unknown atlas-io primitive".to_string());
            }
            let mut args = args.into_iter();
            let url = exec.whnf_at(args.next().expect("fetch URL argument")).await;
            let hash = exec
                .whnf_at(args.next().expect("fetch hash argument"))
                .await;
            let url = match &*url.view() {
                Term::Box(value) => match exec.heap.value_get(value) {
                    Boxed::Str(url) => url.clone(),
                    _ => return Err("%fetch expects its URL to be a String".to_string()),
                },
                _ => return Err("%fetch expects its URL to be a String".to_string()),
            };
            let hash = match &*hash.view() {
                Term::Box(value) => match exec.heap.value_get(value) {
                    Boxed::Str(hash) => hash.clone(),
                    _ => return Err("%fetch expects its hash to be a String".to_string()),
                },
                _ => return Err("%fetch expects its hash to be a String".to_string()),
            };
            let (algorithm, expected) = HashAlgorithm::parse(&hash)?;
            let response = reqwest::get(url.as_ref())
                .await
                .map_err(|error| format!("%fetch could not fetch {url:?}: {error}"))?
                .error_for_status()
                .map_err(|error| format!("%fetch could not fetch {url:?}: {error}"))?;
            let bytes = response
                .bytes()
                .await
                .map_err(|error| format!("%fetch could not read {url:?}: {error}"))?;
            let actual = algorithm.digest(&bytes);
            if actual != expected {
                return Err(format!(
                    "%fetch hash mismatch for {url:?}: expected {hash}, got {}-{}",
                    match algorithm {
                        HashAlgorithm::Sha256 => "sha256",
                        HashAlgorithm::Md5 => "md5",
                    },
                    hex::encode(actual)
                ));
            }
            Ok(Handle::new(
                exec.heap.alloc(Term::Box(
                    exec.heap.value(Boxed::Bytes(Arc::from(bytes.as_ref()))),
                )),
                exec.heap,
            ))
        })
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    use super::*;

    fn serve(status: &'static str, body: &'static [u8]) -> (String, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0; 1024];
            assert!(stream.read(&mut request).unwrap() > 0);
            write!(
                stream,
                "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .unwrap();
            stream.write_all(body).unwrap();
        });
        (format!("http://{address}/value"), server)
    }

    #[test]
    fn fetches_sha256_verified_bytes() {
        let (url, server) = serve("200 OK", b"hello");
        let hash = format!("sha256-{}", hex::encode(sha2::Sha256::digest(b"hello")));
        assert_eq!(
            atlas_core::vm::run_with(&format!(r#"%fetch {url:?} {hash:?}"#), &IoExtensions)
                .unwrap(),
            "[104, 101, 108, 108, 111]"
        );
        server.join().unwrap();
    }

    #[test]
    fn fetches_md5_verified_bytes() {
        let (url, server) = serve("200 OK", b"hello");
        let hash = format!("md5-{}", hex::encode(md5::Md5::digest(b"hello")));
        assert_eq!(
            atlas_core::vm::run_with(&format!(r#"%fetch {url:?} {hash:?}"#), &IoExtensions)
                .unwrap(),
            "[104, 101, 108, 108, 111]"
        );
        server.join().unwrap();
    }

    #[test]
    fn rejects_hash_mismatches_and_http_errors() {
        let (url, server) = serve("200 OK", b"hello");
        let error = atlas_core::vm::run_with(
            &format!(r#"%fetch {url:?} "sha256-{}""#, "00".repeat(32)),
            &IoExtensions,
        )
        .unwrap_err();
        assert!(error.contains("hash mismatch"), "got: {error}");
        server.join().unwrap();

        let (url, server) = serve("404 Not Found", b"missing");
        let error = atlas_core::vm::run_with(
            &format!(r#"%fetch {url:?} "sha256-{}""#, "00".repeat(32)),
            &IoExtensions,
        )
        .unwrap_err();
        assert!(error.contains("404"), "got: {error}");
        server.join().unwrap();
    }

    #[test]
    fn validates_hash_and_argument_types_before_fetching() {
        let cases = [
            (r#"%fetch 1 "sha256-00""#, "URL to be a String"),
            (r#"%fetch "http://localhost" 1"#, "hash to be a String"),
            (
                r#"%fetch "http://localhost" "sha1-00""#,
                "does not support hash algorithm",
            ),
            (
                r#"%fetch "http://localhost" "sha256-xx""#,
                "not valid hexadecimal",
            ),
            (r#"%fetch "http://localhost" "md5-00""#, "md5 requires 16"),
        ];
        for (source, expected) in cases {
            let error = atlas_core::vm::run_with(source, &IoExtensions).unwrap_err();
            assert!(error.contains(expected), "got: {error}");
        }
    }
}
