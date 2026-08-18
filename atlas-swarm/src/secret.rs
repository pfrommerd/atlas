//! Encryption for secrets replicated inside managed service configurations.

use std::fmt;

use base64::{Engine, engine::general_purpose::STANDARD_NO_PAD};
use hpke::{
    Deserializable, Kem as KemTrait, OpModeR, OpModeS, Serializable, aead::ChaCha20Poly1305,
    kdf::HkdfSha256, kem::X25519HkdfSha256,
};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as _};
use zeroize::Zeroizing;

type Kem = X25519HkdfSha256;
type Aead = ChaCha20Poly1305;
type Kdf = HkdfSha256;

const HPKE_INFO: &[u8] = b"atlas-swarm/unit-secret/v1";

#[derive(Clone, Copy, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct EncryptionPublicKey(pub [u8; 32]);

impl fmt::Debug for EncryptionPublicKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("EncryptionPublicKey")
            .field(&STANDARD_NO_PAD.encode(self.0))
            .finish()
    }
}

impl Serialize for EncryptionPublicKey {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&STANDARD_NO_PAD.encode(self.0))
    }
}

impl<'de> Deserialize<'de> for EncryptionPublicKey {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let encoded = String::deserialize(deserializer)?;
        let bytes = STANDARD_NO_PAD.decode(encoded).map_err(D::Error::custom)?;
        let bytes = <[u8; 32]>::try_from(bytes)
            .map_err(|_| D::Error::custom("an encryption public key must be 32 bytes"))?;
        Ok(Self(bytes))
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "scheme", rename_all = "kebab-case")]
pub enum EncryptedSecret {
    HpkeX25519HkdfSha256ChaCha20Poly1305 {
        recipient: EncryptionPublicKey,
        encapsulated_key: String,
        ciphertext: String,
    },
}

pub(crate) fn generate_encryption_keypair() -> ([u8; 32], EncryptionPublicKey) {
    let (secret, public) = Kem::gen_keypair();
    (
        secret
            .to_bytes()
            .as_slice()
            .try_into()
            .expect("X25519 private keys are 32 bytes"),
        EncryptionPublicKey(
            public
                .to_bytes()
                .as_slice()
                .try_into()
                .expect("X25519 public keys are 32 bytes"),
        ),
    )
}

pub(crate) fn public_key_from_secret(secret: &[u8; 32]) -> EncryptionPublicKey {
    let secret = <Kem as KemTrait>::PrivateKey::from_bytes(secret)
        .expect("stored X25519 private keys are valid");
    let public = Kem::sk_to_pk(&secret);
    EncryptionPublicKey(
        public
            .to_bytes()
            .as_slice()
            .try_into()
            .expect("X25519 public keys are 32 bytes"),
    )
}

impl EncryptedSecret {
    pub fn seal(
        recipient: EncryptionPublicKey,
        plaintext: &[u8],
        associated_data: &[u8],
    ) -> Result<Self, String> {
        let public = <Kem as KemTrait>::PublicKey::from_bytes(&recipient.0)
            .map_err(|_| "invalid encryption recipient".to_owned())?;
        let (encapsulated_key, mut context) =
            hpke::setup_sender::<Aead, Kdf, Kem>(&OpModeS::Base, &public, HPKE_INFO)
                .map_err(|_| "failed to initialize secret encryption".to_owned())?;
        let ciphertext = context
            .seal(plaintext, associated_data)
            .map_err(|_| "failed to encrypt secret".to_owned())?;
        Ok(Self::HpkeX25519HkdfSha256ChaCha20Poly1305 {
            recipient,
            encapsulated_key: STANDARD_NO_PAD.encode(encapsulated_key.to_bytes()),
            ciphertext: STANDARD_NO_PAD.encode(ciphertext),
        })
    }

    pub(crate) fn open(
        &self,
        recipient_secret: &[u8; 32],
        associated_data: &[u8],
    ) -> Result<Zeroizing<Vec<u8>>, String> {
        let Self::HpkeX25519HkdfSha256ChaCha20Poly1305 {
            recipient,
            encapsulated_key,
            ciphertext,
        } = self;
        if public_key_from_secret(recipient_secret) != *recipient {
            return Err("encrypted secret is intended for another node".into());
        }
        let secret = <Kem as KemTrait>::PrivateKey::from_bytes(recipient_secret)
            .map_err(|_| "invalid node encryption key".to_owned())?;
        let encapsulated_key = STANDARD_NO_PAD
            .decode(encapsulated_key)
            .map_err(|_| "invalid encapsulated key encoding".to_owned())?;
        let encapsulated_key = <Kem as KemTrait>::EncappedKey::from_bytes(&encapsulated_key)
            .map_err(|_| "invalid encapsulated key".to_owned())?;
        let ciphertext = STANDARD_NO_PAD
            .decode(ciphertext)
            .map_err(|_| "invalid ciphertext encoding".to_owned())?;
        let mut context = hpke::setup_receiver::<Aead, Kdf, Kem>(
            &OpModeR::Base,
            &secret,
            &encapsulated_key,
            HPKE_INFO,
        )
        .map_err(|_| "failed to initialize secret decryption".to_owned())?;
        context
            .open(&ciphertext, associated_data)
            .map(Zeroizing::new)
            .map_err(|_| "encrypted secret authentication failed".to_owned())
    }
}

pub fn secret_associated_data(unit_path: &crate::SwarmPath, environment_name: &str) -> Vec<u8> {
    serde_cbor::to_vec(&(
        b"atlas-swarm/unit-environment/v1",
        unit_path,
        environment_name,
    ))
    .expect("secret associated data serialization cannot fail")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hpke_round_trip_binds_recipient_and_context() {
        let (secret, public) = generate_encryption_keypair();
        let (_, other_public) = generate_encryption_keypair();
        let aad = b"unit and variable";
        let envelope = EncryptedSecret::seal(public, b"private material", aad).unwrap();
        assert_eq!(&*envelope.open(&secret, aad).unwrap(), b"private material");
        assert!(envelope.open(&secret, b"other context").is_err());
        let (other_secret, _) = generate_encryption_keypair();
        assert!(envelope.open(&other_secret, aad).is_err());
        assert_ne!(public, other_public);
        assert!(
            !serde_json::to_string(&envelope)
                .unwrap()
                .contains("private material")
        );
    }
}
