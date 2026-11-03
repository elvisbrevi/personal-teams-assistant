//! Handing the account content key and the account's service token to a computer the user is
//! trusting (decision 18 of the cloud storage design), end to end between the two installations:
//! the portal, the tunnel and Cloudflare carry only ciphertext. The computer that signs in
//! generates an X25519 key pair and keeps the private key in memory; its public key is part of the
//! short code both screens show, so a key swapped in transit shows another code. The coordinator
//! seals the bundle to that public key with an ephemeral key of its own (X25519, HKDF-SHA256 and
//! AES-256-GCM-SIV), bound to the sign-in it answers.

use aes_gcm_siv::{
    Aes256GcmSiv, KeyInit, Nonce,
    aead::{Aead, Payload},
};
use anyhow::{Context, Result, anyhow, ensure};
use aws_lc_rs::agreement::{self, PrivateKey, UnparsedPublicKey, X25519};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::seal::hmac_sha256;

const DOMAIN: &[u8] = b"personal-teams-assistant/cloud-handoff/v1";
/// The largest bundle: the content key file and a service token are a few kilobytes.
const MAX_BYTES: usize = 64 * 1024;

/// A sealed bundle as it travels: the sender's ephemeral public key, the nonce and the ciphertext.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Sealed {
    pub key: String,
    pub nonce: String,
    pub data: String,
}

/// The receiving side: a key pair for one sign-in, never written anywhere.
pub struct Receiver {
    key: PrivateKey,
    public: String,
}

impl Receiver {
    pub fn generate() -> Result<Self> {
        let key = PrivateKey::generate(&X25519).map_err(|_| anyhow!("no key pair"))?;
        let public = hex(key
            .compute_public_key()
            .map_err(|_| anyhow!("no public key"))?
            .as_ref());
        Ok(Self { key, public })
    }

    /// The public key, 64 hex digits, sent with the sign-in.
    pub fn public(&self) -> &str {
        &self.public
    }

    /// Open a bundle sealed to this key for `context` (the sign-in it answers).
    pub fn open(&self, context: &str, sealed: &Sealed) -> Result<Vec<u8>> {
        let sender = unhex(&sealed.key, 32).context("invalid handoff")?;
        let nonce = unhex(&sealed.nonce, 12).context("invalid handoff")?;
        let data = STANDARD.decode(&sealed.data).context("invalid handoff")?;
        ensure!(data.len() <= MAX_BYTES + 16, "invalid handoff");
        let key = agreement::agree(
            &self.key,
            UnparsedPublicKey::new(&X25519, &sender),
            anyhow!("invalid handoff"),
            |secret| Ok(derive(secret, context, &sealed.key, &self.public)),
        )?;
        let aad = associated(context, &sealed.key, &self.public);
        Aes256GcmSiv::new_from_slice(&key)
            .map_err(|_| anyhow!("invalid handoff"))?
            .decrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: &data,
                    aad: &aad,
                },
            )
            .map_err(|_| anyhow!("The handoff could not be opened on this computer."))
    }
}

/// Seal `plaintext` to `recipient` (its public key in hex) for `context`.
pub fn seal(recipient: &str, context: &str, plaintext: &[u8]) -> Result<Sealed> {
    ensure!(plaintext.len() <= MAX_BYTES, "the handoff is too large");
    let peer = unhex(recipient, 32).context("invalid public key")?;
    let ephemeral = PrivateKey::generate(&X25519).map_err(|_| anyhow!("no key pair"))?;
    let sender = hex(ephemeral
        .compute_public_key()
        .map_err(|_| anyhow!("no public key"))?
        .as_ref());
    let key = agreement::agree(
        &ephemeral,
        UnparsedPublicKey::new(&X25519, &peer),
        anyhow!("invalid public key"),
        |secret| Ok(derive(secret, context, &sender, recipient)),
    )?;
    let nonce: [u8; 12] = rand::random();
    let aad = associated(context, &sender, recipient);
    let data = Aes256GcmSiv::new_from_slice(&key)
        .map_err(|_| anyhow!("invalid key"))?
        .encrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: plaintext,
                aad: &aad,
            },
        )
        .map_err(|_| anyhow!("the handoff could not be sealed"))?;
    Ok(Sealed {
        key: sender,
        nonce: hex(&nonce),
        data: STANDARD.encode(data),
    })
}

/// The short code both screens show (`XXXX-XXXX`): from the sign-in's public handle and, when
/// the computer asked for the account store, its public key.
pub fn code(transaction: &str, recipient: Option<&str>) -> String {
    let digest = match recipient {
        None => hex(&Sha256::digest(transaction.as_bytes())),
        Some(key) => hex(&Sha256::digest(format!("{transaction}:{key}").as_bytes())),
    }
    .to_ascii_uppercase();
    format!("{}-{}", &digest[..4], &digest[4..8])
}

/// HKDF-SHA256 (RFC 5869) of the shared secret, salted with the domain, for one sign-in and one
/// pair of public keys.
fn derive(secret: &[u8], context: &str, sender: &str, recipient: &str) -> [u8; 32] {
    let prk = hmac_sha256(DOMAIN, &[secret]);
    hmac_sha256(&prk, &[&associated(context, sender, recipient), &[1]])
}

fn associated(context: &str, sender: &str, recipient: &str) -> Vec<u8> {
    let mut out = DOMAIN.to_vec();
    for part in [context, sender, recipient] {
        out.extend_from_slice(&(part.len() as u32).to_be_bytes());
        out.extend_from_slice(part.as_bytes());
    }
    out
}

pub fn valid_public(value: &str) -> bool {
    unhex(value, 32).is_some()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(text: &str, len: usize) -> Option<Vec<u8>> {
    if text.len() != len * 2 || !text.bytes().all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f')) {
        return None;
    }
    (0..len)
        .map(|i| u8::from_str_radix(&text[i * 2..i * 2 + 2], 16).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_receiver_opens_a_bundle_for_its_sign_in() {
        let receiver = Receiver::generate().unwrap();
        assert!(valid_public(receiver.public()));
        let sealed = seal(receiver.public(), "sign-in-1", b"synthetic bundle").unwrap();
        assert_eq!(
            receiver.open("sign-in-1", &sealed).unwrap(),
            b"synthetic bundle"
        );
        // Another sign-in, another receiver or a changed byte fails.
        assert!(receiver.open("sign-in-2", &sealed).is_err());
        let other = Receiver::generate().unwrap();
        assert!(other.open("sign-in-1", &sealed).is_err());
        let mut tampered = sealed.clone();
        tampered.nonce = hex(&[0u8; 12]);
        assert!(receiver.open("sign-in-1", &tampered).is_err());
        // Each seal uses a fresh ephemeral key and nonce.
        let again = seal(receiver.public(), "sign-in-1", b"synthetic bundle").unwrap();
        assert_ne!(again.key, sealed.key);
        assert!(seal("00", "sign-in-1", b"x").is_err());
    }

    #[test]
    fn the_code_covers_the_public_key() {
        let transaction = "a".repeat(64);
        let plain = code(&transaction, None);
        assert_eq!(plain.len(), 9);
        let one = code(&transaction, Some(&"1".repeat(64)));
        let two = code(&transaction, Some(&"2".repeat(64)));
        assert_ne!(one, two);
        assert_ne!(one, plain);
    }
}
