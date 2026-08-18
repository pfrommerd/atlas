use std::{io, path::PathBuf};

use atlas_swarm::BinaryBlob;

const DAEMON: BinaryBlob = BinaryBlob::new(
    include_bytes!(env!("ATLAS_DAEMON_ARTIFACT")),
    env!("ATLAS_DAEMON_SHA256"),
);

pub fn extract() -> io::Result<PathBuf> {
    DAEMON.path()
}

#[cfg(test)]
mod tests {
    #[test]
    fn artifact_hashes_are_sha256_cache_keys() {
        assert_eq!(env!("ATLAS_DAEMON_SHA256").len(), 64);
    }
}
