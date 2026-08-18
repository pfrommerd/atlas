//! SSH-agent backed Atlas Ed25519 signing.

use std::{io, path::PathBuf};

use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixStream,
};

use crate::{SECURITY_KEY_APPLICATION, UserId, UserSignature};

/// Atlas signer backed by ssh-agent, falling back to an unencrypted OpenSSH
/// Ed25519 private key only when the agent has no eligible identity.
#[derive(Clone)]
pub enum UserSigner {
    Agent(SshAgentSigner),
    File(ed25519_dalek::SigningKey),
}

impl UserSigner {
    pub async fn discover() -> io::Result<Self> {
        match SshAgentSigner::discover().await {
            Ok(signer) => Ok(Self::Agent(signer)),
            Err(agent_error)
                if matches!(
                    agent_error.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
                ) =>
            {
                Self::from_default_file()
            }
            Err(error) => Err(error),
        }
    }

    fn from_default_file() -> io::Result<Self> {
        let home = std::env::var_os("HOME")
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "HOME is not set"))?;
        let source = std::fs::read(PathBuf::from(home).join(".ssh/id_ed25519"))?;
        let key = ssh_key::PrivateKey::from_openssh(source)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        let private = key.key_data().ed25519().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "SSH fallback key is not Ed25519",
            )
        })?;
        Ok(Self::File(ed25519_dalek::SigningKey::from_bytes(
            &private.private.to_bytes(),
        )))
    }

    pub fn user(&self) -> UserId {
        match self {
            Self::Agent(signer) => signer.user(),
            Self::File(key) => UserId::from_signing_key(key),
        }
    }

    pub async fn sign(&self, payload: &[u8]) -> io::Result<UserSignature> {
        match self {
            Self::Agent(signer) => signer.sign(payload).await,
            Self::File(key) => Ok(UserSignature::Ed25519(
                ed25519_dalek::Signer::sign(key, payload)
                    .to_bytes()
                    .to_vec(),
            )),
        }
    }
}

/// An `ssh-ed25519` identity held by the local SSH agent.
#[derive(Clone)]
pub struct SshAgentSigner {
    socket: PathBuf,
    key_blob: Vec<u8>,
    user: UserId,
    security_key: bool,
}

impl SshAgentSigner {
    /// Selects the first ordinary Ed25519 identity exposed by `SSH_AUTH_SOCK`.
    pub async fn discover() -> io::Result<Self> {
        let socket = std::env::var_os("SSH_AUTH_SOCK")
            .map(PathBuf::from)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "SSH_AUTH_SOCK is not set"))?;
        let mut stream = UnixStream::connect(&socket).await?;
        write_packet(&mut stream, &[11]).await?;
        let response = read_packet(&mut stream).await?;
        if response.first() != Some(&12) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "SSH agent rejected identity request",
            ));
        }
        let mut input = &response[1..];
        let count = take_u32(&mut input)?;
        for _ in 0..count {
            let key_blob = take_string(&mut input)?.to_vec();
            let _comment = take_string(&mut input)?;
            let mut key = key_blob.as_slice();
            let algorithm = take_string(&mut key)?;
            let public_key = take_string(&mut key)?;
            let Ok(public_key) = <[u8; 32]>::try_from(public_key) else {
                continue;
            };
            let security_key = algorithm == b"sk-ssh-ed25519@openssh.com";
            if algorithm != b"ssh-ed25519" && !security_key {
                continue;
            }
            if security_key && take_string(&mut key)? != SECURITY_KEY_APPLICATION.as_bytes() {
                continue;
            }
            if !key.is_empty() {
                continue;
            }
            return Ok(Self {
                socket,
                key_blob,
                user: UserId(public_key),
                security_key,
            });
        }
        Err(io::Error::new(
            io::ErrorKind::NotFound,
            "SSH agent has no ssh-ed25519 identity",
        ))
    }

    pub fn user(&self) -> UserId {
        self.user
    }

    /// Signs an Atlas domain-separated payload through ssh-agent.
    pub async fn sign(&self, payload: &[u8]) -> io::Result<UserSignature> {
        let mut request = vec![13];
        put_string(&mut request, &self.key_blob);
        put_string(&mut request, payload);
        request.extend_from_slice(&0u32.to_be_bytes());
        let mut stream = UnixStream::connect(&self.socket).await?;
        write_packet(&mut stream, &request).await?;
        let response = read_packet(&mut stream).await?;
        if response.first() != Some(&14) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "SSH agent refused signature",
            ));
        }
        let mut signature_blob = take_string(&mut &response[1..])?;
        let algorithm = take_string(&mut signature_blob)?;
        if (!self.security_key && algorithm != b"ssh-ed25519")
            || (self.security_key && algorithm != b"sk-ssh-ed25519@openssh.com")
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "SSH agent returned a non-Ed25519 signature",
            ));
        }
        let signature = take_string(&mut signature_blob)?;
        if signature.len() != 64 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "SSH agent returned an invalid Ed25519 signature",
            ));
        }
        if !self.security_key {
            if !signature_blob.is_empty() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "unexpected SSH signature fields",
                ));
            }
            return Ok(UserSignature::Ed25519(signature.to_vec()));
        }
        let flags = *signature_blob.first().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "truncated security-key signature",
            )
        })?;
        signature_blob = &signature_blob[1..];
        let counter = take_u32(&mut signature_blob)?;
        if !signature_blob.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unexpected security-key signature fields",
            ));
        }
        Ok(UserSignature::SecurityKeyEd25519 {
            flags,
            counter,
            signature: signature.to_vec(),
        })
    }
}

async fn write_packet(stream: &mut UnixStream, packet: &[u8]) -> io::Result<()> {
    stream
        .write_all(&(packet.len() as u32).to_be_bytes())
        .await?;
    stream.write_all(packet).await
}

async fn read_packet(stream: &mut UnixStream) -> io::Result<Vec<u8>> {
    let mut length = [0; 4];
    stream.read_exact(&mut length).await?;
    let mut packet = vec![0; u32::from_be_bytes(length) as usize];
    stream.read_exact(&mut packet).await?;
    Ok(packet)
}

fn take_u32(input: &mut &[u8]) -> io::Result<u32> {
    let bytes: [u8; 4] = input
        .get(..4)
        .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "truncated SSH agent message"))?
        .try_into()
        .unwrap();
    *input = &input[4..];
    Ok(u32::from_be_bytes(bytes))
}

fn take_string<'a>(input: &mut &'a [u8]) -> io::Result<&'a [u8]> {
    let length = take_u32(input)? as usize;
    let value = input.get(..length).ok_or_else(|| {
        io::Error::new(io::ErrorKind::UnexpectedEof, "truncated SSH agent string")
    })?;
    *input = &input[length..];
    Ok(value)
}

fn put_string(output: &mut Vec<u8>, value: &[u8]) {
    output.extend_from_slice(&(value.len() as u32).to_be_bytes());
    output.extend_from_slice(value);
}
