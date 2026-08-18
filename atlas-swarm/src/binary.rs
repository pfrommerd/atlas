//! Execution of content-addressed binary blobs.

use std::{
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    process::Command,
};

#[derive(Clone, Copy)]
pub struct BinaryBlob {
    bytes: &'static [u8],
    hash: &'static str,
}

impl BinaryBlob {
    pub const fn new(bytes: &'static [u8], hash: &'static str) -> Self {
        Self { bytes, hash }
    }

    pub fn path(self) -> io::Result<PathBuf> {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "HOME is not set"))?;
        self.extract_to(&home.join(".cache/atlas/bin"))
    }

    pub fn command(self) -> io::Result<Command> {
        self.path().map(Command::new)
    }

    #[cfg(test)]
    fn command_in(self, directory: &Path) -> io::Result<Command> {
        self.extract_to(directory).map(Command::new)
    }

    fn extract_to(self, directory: &Path) -> io::Result<PathBuf> {
        if self.hash.len() != 64 || !self.hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "binary blob hash must be 64 hexadecimal characters",
            ));
        }
        fs::create_dir_all(directory)?;
        #[cfg(unix)]
        fs::set_permissions(
            directory,
            std::os::unix::fs::PermissionsExt::from_mode(0o700),
        )?;
        let path = directory.join(self.hash);
        if path.exists() {
            return Ok(path);
        }

        let temporary = directory.join(format!(".{}.{}.tmp", self.hash, uuid::Uuid::new_v4()));
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)?;
        if let Err(error) = file.write_all(self.bytes).and_then(|()| file.sync_all()) {
            drop(file);
            let _ = fs::remove_file(&temporary);
            return Err(error);
        }
        #[cfg(unix)]
        file.set_permissions(std::os::unix::fs::PermissionsExt::from_mode(0o700))?;
        drop(file);
        match fs::rename(&temporary, &path) {
            Ok(()) => Ok(path),
            Err(_error) if path.exists() => {
                let _ = fs::remove_file(temporary);
                Ok(path)
            }
            Err(error) => {
                let _ = fs::remove_file(temporary);
                Err(error)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HASH: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    #[test]
    fn extracts_once_without_verifying_an_existing_cache_entry() {
        let directory =
            std::env::temp_dir().join(format!("atlas-binary-test-{}", uuid::Uuid::new_v4()));
        let first = BinaryBlob::new(b"first", HASH)
            .extract_to(&directory)
            .unwrap();
        let second = BinaryBlob::new(b"different", HASH)
            .extract_to(&directory)
            .unwrap();
        assert_eq!(first, second);
        assert_eq!(fs::read(&first).unwrap(), b"first");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                first.metadata().unwrap().permissions().mode() & 0o777,
                0o700
            );
        }
        assert_eq!(
            BinaryBlob::new(b"ignored", HASH)
                .command_in(&directory)
                .unwrap()
                .get_program(),
            first.as_os_str()
        );
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn rejects_a_hash_that_could_escape_the_cache_directory() {
        let blob = BinaryBlob::new(b"value", "../../escape");
        let directory =
            std::env::temp_dir().join(format!("atlas-binary-test-{}", uuid::Uuid::new_v4()));
        assert_eq!(
            blob.extract_to(&directory).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
    }
}
