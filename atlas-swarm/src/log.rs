use std::{collections::{BTreeMap, BTreeSet}, fmt, str::FromStr};

use ed25519_dalek::{Signature as EdSignature, Signer, SigningKey, Verifier, VerifyingKey};
use iroh::{EndpointAddr, EndpointId, SecretKey, Signature};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const SECURITY_KEY_APPLICATION: &str = "atlas-swarm:v1";

pub type CommitId = Uuid;

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct NodeCoordinate {
    pub x: f64,
    pub y: f64,
}

impl NodeCoordinate {
    pub fn new(x: f64, y: f64) -> Option<Self> {
        (x.is_finite() && y.is_finite()).then_some(Self { x, y })
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NodeRecord {
    pub name: String,
    pub endpoint_id: EndpointId,
    pub endpoint_addr: EndpointAddr,
    pub coordinate: NodeCoordinate,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct UserId(pub [u8; 32]);

impl UserId {
    pub fn from_signing_key(key: &SigningKey) -> Self {
        Self(key.verifying_key().to_bytes())
    }
    pub fn verifying_key(self) -> Option<VerifyingKey> {
        VerifyingKey::from_bytes(&self.0).ok()
    }
}

impl fmt::Display for UserId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 { write!(formatter, "{byte:02x}")?; }
        Ok(())
    }
}

impl FromStr for UserId {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.len() != 64 { return Err("an Atlas user id must be 64 hexadecimal characters".into()); }
        let mut bytes = [0; 32];
        for (index, byte) in bytes.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16)
                .map_err(|_| "an Atlas user id must be hexadecimal")?;
        }
        Ok(Self(bytes))
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserMetadata {
    pub username: Option<String>,
    pub real_name: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedUserMetadata {
    pub user: UserId,
    pub metadata: UserMetadata,
    pub signature: UserSignature,
}

impl SignedUserMetadata {
    pub fn new(metadata: UserMetadata, key: &SigningKey) -> Self {
        let user = UserId::from_signing_key(key);
        let signature = UserSignature::Ed25519(key.sign(&user_metadata_bytes(user, &metadata)).to_bytes().to_vec());
        Self {
            user,
            metadata,
            signature,
        }
    }

    pub fn verify(&self) -> bool {
        self.signature.verify(self.user, &user_metadata_bytes(self.user, &self.metadata))
    }
}

fn user_metadata_bytes(user: UserId, metadata: &UserMetadata) -> Vec<u8> {
    serde_cbor::to_vec(&(b"atlas-swarm/user-metadata/1", user, metadata))
        .expect("metadata serialization cannot fail")
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct SwarmPath(String);

impl SwarmPath {
    pub fn new(path: impl Into<String>) -> Option<Self> {
        let path = path.into();
        (!path.is_empty()
            && path
                .split('/')
                .all(|part| !part.is_empty() && part != "." && part != ".."))
        .then_some(Self(path))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

pub type ServicePath = SwarmPath;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PathAcl {
    pub readers: BTreeSet<UserId>,
    pub writers: BTreeSet<UserId>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepositoryRecord {
    pub endpoints: BTreeSet<EndpointId>,
    pub allowed_users: BTreeSet<UserId>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceRecord {
    pub provider: EndpointId,
    pub allowed_users: BTreeSet<UserId>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PathResource {
    Service(ServiceRecord),
    Repository(RepositoryRecord),
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PathEntry {
    pub acl: Option<PathAcl>,
    pub resource: Option<PathResource>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PathOperation {
    SetAcl {
        path: Option<SwarmPath>,
        acl: PathAcl,
    },
    DefineService {
        path: SwarmPath,
        service: ServiceRecord,
    },
    DefineRepository {
        path: SwarmPath,
        repository: RepositoryRecord,
    },
    RemoveResource {
        path: SwarmPath,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedPathOperation {
    pub user: UserId,
    pub operation: PathOperation,
    pub signature: UserSignature,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum UserSignature {
    Ed25519(Vec<u8>),
    SecurityKeyEd25519 { flags: u8, counter: u32, signature: Vec<u8> },
}

impl UserSignature {
    pub fn verify(&self, user: UserId, payload: &[u8]) -> bool {
        let Some(key) = user.verifying_key() else { return false };
        match self {
            Self::Ed25519(signature) => signature.as_slice().try_into().is_ok_and(|signature| key.verify(payload, &EdSignature::from_bytes(signature)).is_ok()),
            Self::SecurityKeyEd25519 { flags, counter, signature } => {
                if flags & 1 == 0 { return false; }
                let Ok(signature) = signature.as_slice().try_into() else { return false; };
                key.verify(&security_key_payload(payload, *flags, *counter), &EdSignature::from_bytes(signature)).is_ok()
            }
        }
    }
}

fn security_key_payload(payload: &[u8], flags: u8, counter: u32) -> Vec<u8> {
    use sha2::{Digest, Sha256};
    let mut result = Vec::with_capacity(32 + 1 + 4 + 32);
    result.extend_from_slice(&Sha256::digest(SECURITY_KEY_APPLICATION.as_bytes()));
    result.push(flags);
    result.extend_from_slice(&counter.to_be_bytes());
    result.extend_from_slice(&Sha256::digest(payload));
    result
}

impl SignedPathOperation {
    pub fn new(operation: PathOperation, key: &SigningKey) -> Self {
        let user = UserId::from_signing_key(key);
        let signature = UserSignature::Ed25519(key.sign(&path_operation_bytes(user, &operation)).to_bytes().to_vec());
        Self {
            user,
            operation,
            signature,
        }
    }

    pub fn verify(&self) -> bool {
        self.signature.verify(self.user, &path_operation_bytes(self.user, &self.operation))
    }

    /// Signs an operation using an ordinary Ed25519 identity held by ssh-agent.
    pub async fn from_ssh_agent(
        operation: PathOperation,
        signer: &crate::auth::UserSigner,
    ) -> Result<Self, std::io::Error> {
        let user = signer.user();
        let signature = signer.sign(&path_operation_bytes(user, &operation)).await?;
        Ok(Self { user, operation, signature })
    }
}

fn path_operation_bytes(user: UserId, operation: &PathOperation) -> Vec<u8> {
    serde_cbor::to_vec(&(b"atlas-swarm/path-operation/1", user, operation))
        .expect("path operation serialization cannot fail")
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum MembershipOperation {
    Join(NodeRecord),
    Rename { name: String },
    MarkDown { node: EndpointId },
    MarkUp,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum SwarmOperation {
    Membership(MembershipOperation),
    UserMetadata(SignedUserMetadata),
    Path(SignedPathOperation),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Commit {
    pub id: CommitId,
    pub parents: BTreeSet<CommitId>,
    pub author: EndpointId,
    pub operation: SwarmOperation,
    pub signature: Vec<u8>,
}

impl Commit {
    pub fn new(
        parents: BTreeSet<CommitId>,
        author: EndpointId,
        operation: impl Into<SwarmOperation>,
        key: &SecretKey,
    ) -> Self {
        let mut commit = Self {
            id: Uuid::new_v4(),
            parents,
            author,
            operation: operation.into(),
            signature: Vec::new(),
        };
        commit.signature = key.sign(&commit.signing_bytes()).to_bytes().to_vec();
        commit
    }

    pub fn verify(&self) -> bool {
        let Ok(bytes) = self.signature.as_slice().try_into() else {
            return false;
        };
        self.author
            .verify(&self.signing_bytes(), &Signature::from_bytes(bytes))
            .is_ok()
    }

    fn signing_bytes(&self) -> Vec<u8> {
        let mut unsigned = self.clone();
        unsigned.signature.clear();
        serde_cbor::to_vec(&unsigned).expect("commit serialization cannot fail")
    }
}

impl From<MembershipOperation> for SwarmOperation {
    fn from(value: MembershipOperation) -> Self {
        Self::Membership(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn security_key_signatures_bind_the_fixed_application_and_user_presence() {
        let key = SigningKey::from_bytes(&[42; 32]);
        let user = UserId::from_signing_key(&key);
        let payload = b"atlas security key test";
        let flags = 1;
        let counter = 7;
        let signature = UserSignature::SecurityKeyEd25519 {
            flags,
            counter,
            signature: key.sign(&security_key_payload(payload, flags, counter)).to_bytes().to_vec(),
        };
        assert!(signature.verify(user, payload));
        assert!(!UserSignature::SecurityKeyEd25519 {
            flags: 0,
            counter,
            signature: key.sign(&security_key_payload(payload, 0, counter)).to_bytes().to_vec(),
        }.verify(user, payload));
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct MembershipView {
    pub nodes: BTreeMap<String, NodeRecord>,
    pub down: BTreeSet<EndpointId>,
}

impl MembershipView {
    pub fn is_down(&self, id: &EndpointId) -> bool {
        self.down.contains(id)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SwarmView {
    pub membership: MembershipView,
    pub users: BTreeMap<UserId, UserMetadata>,
    pub root_acl: Option<PathAcl>,
    pub paths: BTreeMap<SwarmPath, PathEntry>,
}
