use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    str::FromStr,
};

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
    pub encryption_key: crate::EncryptionPublicKey,
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
        for byte in self.0 {
            write!(formatter, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl FromStr for UserId {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.len() != 64 {
            return Err("an Atlas user id must be 64 hexadecimal characters".into());
        }
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

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct SwarmPath(String);

impl SwarmPath {
    pub fn new(path: impl Into<String>) -> Option<Self> {
        let path = path.into();
        (path == "/"
            || (path.starts_with('/')
                && path.strip_prefix('/').is_some_and(|path| {
                    path.split('/')
                        .all(|part| !part.is_empty() && part != "." && part != "..")
                })))
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
    #[serde(default)]
    pub endpoint_addr: Option<iroh::EndpointAddr>,
    pub allowed_users: BTreeSet<UserId>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum PathResource {
    Node(NodeRecord),
    Service(ServiceRecord),
    Repository(RepositoryRecord),
    Config(serde_json::Value),
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PathEntry {
    pub acl: Option<PathAcl>,
    pub resource: Option<PathResource>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum PathOperation {
    SetAcl {
        path: SwarmPath,
        acl: PathAcl,
    },
    NodeJoin {
        path: SwarmPath,
        node: NodeRecord,
    },
    NodeMove {
        node: EndpointId,
        from: SwarmPath,
        to: SwarmPath,
    },
    DefineService {
        path: SwarmPath,
        service: ServiceRecord,
    },
    DefineRepository {
        path: SwarmPath,
        repository: RepositoryRecord,
    },
    SetConfig {
        path: SwarmPath,
        value: serde_json::Value,
    },
    Remove {
        path: SwarmPath,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum UserSignature {
    Ed25519(Vec<u8>),
    SecurityKeyEd25519 {
        flags: u8,
        counter: u32,
        signature: Vec<u8>,
    },
}

impl UserSignature {
    pub fn verify(&self, user: UserId, payload: &[u8]) -> bool {
        let Some(key) = user.verifying_key() else {
            return false;
        };
        match self {
            Self::Ed25519(signature) => signature.as_slice().try_into().is_ok_and(|signature| {
                key.verify(payload, &EdSignature::from_bytes(signature))
                    .is_ok()
            }),
            Self::SecurityKeyEd25519 {
                flags,
                counter,
                signature,
            } => {
                if flags & 1 == 0 {
                    return false;
                }
                let Ok(signature) = signature.as_slice().try_into() else {
                    return false;
                };
                key.verify(
                    &security_key_payload(payload, *flags, *counter),
                    &EdSignature::from_bytes(signature),
                )
                .is_ok()
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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum MembershipOperation {
    Join(NodeRecord),
    Rename { name: String },
    MarkDown { node: EndpointId },
    MarkUp,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum SwarmOperation {
    Genesis { swarm_id: Uuid, root_acl: PathAcl },
    Membership(MembershipOperation),
    UserMetadata(UserMetadata),
    Path(PathOperation),
    PathBatch(Vec<PathOperation>),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Commit {
    pub id: CommitId,
    pub parents: BTreeSet<CommitId>,
    pub author: EndpointId,
    pub user: UserId,
    pub created_at_ms: u64,
    pub operation: SwarmOperation,
    pub user_signature: UserSignature,
    pub endpoint_signature: Vec<u8>,
}

impl Commit {
    pub fn new_unsigned(
        parents: BTreeSet<CommitId>,
        author: EndpointId,
        user: UserId,
        created_at_ms: u64,
        operation: SwarmOperation,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            parents,
            author,
            user,
            created_at_ms,
            operation,
            user_signature: UserSignature::Ed25519(Vec::new()),
            endpoint_signature: Vec::new(),
        }
    }

    pub fn signing_bytes(&self) -> Vec<u8> {
        let mut unsigned = self.clone();
        unsigned.user_signature = UserSignature::Ed25519(Vec::new());
        unsigned.endpoint_signature.clear();
        serde_cbor::to_vec(&unsigned).expect("commit serialization cannot fail")
    }

    pub fn verify_user(&self) -> bool {
        self.user_signature.verify(self.user, &self.signing_bytes())
    }

    pub fn verify(&self) -> bool {
        let Ok(bytes) = self.endpoint_signature.as_slice().try_into() else {
            return false;
        };
        self.verify_user()
            && self
                .author
                .verify(&self.signing_bytes(), &Signature::from_bytes(bytes))
                .is_ok()
    }

    pub fn sign_user(&mut self, key: &SigningKey) {
        assert_eq!(self.user, UserId::from_signing_key(key));
        self.user_signature =
            UserSignature::Ed25519(key.sign(&self.signing_bytes()).to_bytes().to_vec());
    }

    pub fn sign_endpoint(&mut self, key: &SecretKey) {
        self.endpoint_signature = key.sign(&self.signing_bytes()).to_bytes().to_vec();
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
    fn swarm_paths_are_absolute_and_allow_the_root() {
        assert!(SwarmPath::new("/").is_some());
        assert!(SwarmPath::new("/nodes/laptop").is_some());
        assert!(SwarmPath::new("nodes/laptop").is_none());
        assert!(SwarmPath::new("//nodes").is_none());
    }

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
            signature: key
                .sign(&security_key_payload(payload, flags, counter))
                .to_bytes()
                .to_vec(),
        };
        assert!(signature.verify(user, payload));
        assert!(
            !UserSignature::SecurityKeyEd25519 {
                flags: 0,
                counter,
                signature: key
                    .sign(&security_key_payload(payload, 0, counter))
                    .to_bytes()
                    .to_vec(),
            }
            .verify(user, payload)
        );
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
    pub swarm_id: Option<Uuid>,
    pub membership: MembershipView,
    pub users: BTreeMap<UserId, UserMetadata>,
    pub root_acl: Option<PathAcl>,
    pub paths: BTreeMap<SwarmPath, PathEntry>,
}
