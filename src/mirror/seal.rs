//! Sealing of one record: AES-256-GCM-SIV under the account's content key, with associated data
//! that binds the domain, the content account, the key version, the collection, the (blinded)
//! record ID, the revision and the payload schema, so ciphertext moved to another account,
//! collection, record or revision fails authentication (the construction of account variables).
//!
//! Two differences from the variables, both because a record is re-sent after an ambiguous
//! failure: the nonce is synthetic (a keyed hash of the associated data and the plaintext, which
//! AES-GCM-SIV is designed for), so sealing the same record at the same revision always gives the
//! same bytes and the Worker answers `unchanged` instead of `conflict`; and the key version is
//! the first four bytes of the sealed record, authenticated through the associated data.

use super::{Collection, key::Content};
use aes_gcm_siv::{
    Aes256GcmSiv, KeyInit, Nonce,
    aead::{Aead, Payload},
};
use anyhow::{Result, anyhow};
use sha2::{Digest, Sha256};

const RECORD_DOMAIN: &[u8] = b"personal-teams-assistant/cloud-record/v1";
const NONCE_DOMAIN: &[u8] = b"personal-teams-assistant/cloud-nonce/v1";
const ID_DOMAIN: &[u8] = b"personal-teams-assistant/cloud-id/v1";

pub fn hmac_sha256(key: &[u8], parts: &[&[u8]]) -> [u8; 32] {
    const BLOCK: usize = 64;
    let mut block = [0u8; BLOCK];
    if key.len() > BLOCK {
        block[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        block[..key.len()].copy_from_slice(key);
    }
    let mut inner = Sha256::new();
    inner.update(block.map(|b| b ^ 0x36));
    for part in parts {
        inner.update(part);
    }
    let mut outer = Sha256::new();
    outer.update(block.map(|b| b ^ 0x5c));
    outer.update(inner.finalize());
    outer.finalize().into()
}

fn length_prefixed(out: &mut Vec<u8>, part: &[u8]) {
    out.extend((part.len() as u32).to_be_bytes());
    out.extend(part);
}

/// The identifier the Worker sees for a record: a keyed hash of its natural ID, so no Teams,
/// work item or note identifier reaches Cloudflare. Stable for the life of the account.
pub fn blind_id(content: &Content, collection: Collection, natural: &str) -> String {
    let mut message = Vec::new();
    length_prefixed(&mut message, collection.name().as_bytes());
    length_prefixed(&mut message, natural.as_bytes());
    hmac_sha256(&content.id_key, &[ID_DOMAIN, &message])
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn aad(
    content: &Content,
    version: u32,
    collection: Collection,
    id: &str,
    revision: u64,
    schema: u32,
) -> Vec<u8> {
    let mut aad = Vec::new();
    length_prefixed(&mut aad, RECORD_DOMAIN);
    length_prefixed(&mut aad, content.account.as_bytes());
    aad.extend(version.to_be_bytes());
    length_prefixed(&mut aad, collection.name().as_bytes());
    length_prefixed(&mut aad, id.as_bytes());
    aad.extend(revision.to_be_bytes());
    aad.extend(schema.to_be_bytes());
    aad
}

/// Seal `plaintext` as the record `id` (already blinded) at `revision`.
pub fn seal(
    content: &Content,
    collection: Collection,
    id: &str,
    revision: u64,
    plaintext: &[u8],
) -> Result<Vec<u8>> {
    let version = content.active;
    let key = content.active_key();
    let aad = aad(
        content,
        version,
        collection,
        id,
        revision,
        collection.schema(),
    );
    let nonce_key = hmac_sha256(key, &[NONCE_DOMAIN]);
    let nonce = hmac_sha256(&nonce_key, &[&aad, plaintext]);
    let cipher = Aes256GcmSiv::new_from_slice(key).map_err(|_| anyhow!("invalid content key"))?;
    let encrypted = cipher
        .encrypt(
            Nonce::from_slice(&nonce[..12]),
            Payload {
                msg: plaintext,
                aad: &aad,
            },
        )
        .map_err(|_| anyhow!("The record could not be encrypted."))?;
    Ok([&version.to_be_bytes()[..], &nonce[..12], &encrypted].concat())
}

/// Open a sealed record, or `None` when it does not authenticate for exactly this account,
/// collection, ID, revision and schema.
pub fn open(
    content: &Content,
    collection: Collection,
    id: &str,
    revision: u64,
    schema: u32,
    sealed: &[u8],
) -> Option<Vec<u8>> {
    if sealed.len() <= 16 {
        return None;
    }
    let version = u32::from_be_bytes(sealed[..4].try_into().ok()?);
    let key = content.key(version)?;
    let aad = aad(content, version, collection, id, revision, schema);
    Aes256GcmSiv::new_from_slice(key)
        .ok()?
        .decrypt(
            Nonce::from_slice(&sealed[4..16]),
            Payload {
                msg: &sealed[16..],
                aad: &aad,
            },
        )
        .ok()
}
