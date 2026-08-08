use std::collections::{BTreeMap, BTreeSet};

use ed25519_dalek::{Signature as UserSignature, Signer, SigningKey, Verifier, VerifyingKey};
use iroh::{EndpointAddr, EndpointId, SecretKey, Signature};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

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
    pub fn from_signing_key(key: &SigningKey) -> Self { Self(key.verifying_key().to_bytes()) }
    pub fn verifying_key(self) -> Option<VerifyingKey> { VerifyingKey::from_bytes(&self.0).ok() }
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
    pub signature: Vec<u8>,
}

impl SignedUserMetadata {
    pub fn new(metadata: UserMetadata, key: &SigningKey) -> Self {
        let user = UserId::from_signing_key(key);
        let signature = key.sign(&user_metadata_bytes(user, &metadata)).to_bytes().to_vec();
        Self { user, metadata, signature }
    }

    pub fn verify(&self) -> bool {
        let Ok(signature) = self.signature.as_slice().try_into() else { return false; };
        self.user.verifying_key().is_some_and(|key| key.verify(&user_metadata_bytes(self.user, &self.metadata), &UserSignature::from_bytes(signature)).is_ok())
    }
}

fn user_metadata_bytes(user: UserId, metadata: &UserMetadata) -> Vec<u8> {
    serde_cbor::to_vec(&(b"atlas-swarm/user-metadata/1", user, metadata)).expect("metadata serialization cannot fail")
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct ServicePath(String);

impl ServicePath {
    pub fn new(path: impl Into<String>) -> Option<Self> {
        let path = path.into();
        (!path.is_empty() && path.split('/').all(|part| !part.is_empty() && part != "." && part != "..")).then_some(Self(path))
    }
    pub fn as_str(&self) -> &str { &self.0 }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceRecord {
    pub path: ServicePath,
    pub provider: EndpointId,
    pub allowed_users: BTreeSet<UserId>,
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
    AdvertiseService(ServiceRecord),
    RemoveService { path: ServicePath, provider: EndpointId },
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
    pub fn new(parents: BTreeSet<CommitId>, author: EndpointId, operation: impl Into<SwarmOperation>, key: &SecretKey) -> Self {
        let mut commit = Self { id: Uuid::new_v4(), parents, author, operation: operation.into(), signature: Vec::new() };
        commit.signature = key.sign(&commit.signing_bytes()).to_bytes().to_vec();
        commit
    }

    pub fn verify(&self) -> bool {
        let Ok(bytes) = self.signature.as_slice().try_into() else { return false };
        self.author.verify(&self.signing_bytes(), &Signature::from_bytes(bytes)).is_ok()
    }

    fn signing_bytes(&self) -> Vec<u8> {
        let mut unsigned = self.clone();
        unsigned.signature.clear();
        serde_cbor::to_vec(&unsigned).expect("commit serialization cannot fail")
    }
}

impl From<MembershipOperation> for SwarmOperation {
    fn from(value: MembershipOperation) -> Self { Self::Membership(value) }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct MembershipView {
    pub nodes: BTreeMap<String, NodeRecord>,
    pub down: BTreeSet<EndpointId>,
}

impl MembershipView {
    pub fn is_down(&self, id: &EndpointId) -> bool {
        self.down.contains(id)
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SwarmView {
    pub membership: MembershipView,
    pub users: BTreeMap<UserId, UserMetadata>,
    pub services: BTreeMap<ServicePath, ServiceRecord>,
}
