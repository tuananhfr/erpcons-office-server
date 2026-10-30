//! Seals link tokens (AES-256-GCM) under the server's key so sharers can copy the link again. The
//! document id is the associated data: a sealed token moved to another document's row does not open.
use std::fmt;

use aes_gcm::{
    Aes256Gcm, KeyInit, Nonce,
    aead::{Aead, Payload},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use ring::rand::{SecureRandom, SystemRandom};
use uuid::Uuid;

use crate::DocError;

const KEY_BYTES: usize = 32;
const NONCE_BYTES: usize = 12;

/// `ERPCONS_LINK_KEY`: 32 random bytes. No `Debug` output of the bytes.
#[derive(Clone, PartialEq, Eq)]
pub struct LinkKey([u8; KEY_BYTES]);

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("a link key is 32 bytes in standard base64 (openssl rand -base64 32)")]
pub struct InvalidLinkKey;

impl LinkKey {
    pub fn from_base64(encoded: &str) -> Result<Self, InvalidLinkKey> {
        let bytes = STANDARD
            .decode(encoded.trim())
            .map_err(|_| InvalidLinkKey)?;
        Ok(Self(bytes.try_into().map_err(|_| InvalidLinkKey)?))
    }

    pub fn from_bytes(bytes: [u8; KEY_BYTES]) -> Self {
        Self(bytes)
    }

    fn cipher(&self) -> Aes256Gcm {
        Aes256Gcm::new(&self.0.into())
    }

    /// The nonce and the sealed token, to store beside the token's hash.
    pub(crate) fn seal(&self, document_id: Uuid, token: &str) -> Result<Sealed, DocError> {
        let mut nonce = [0u8; NONCE_BYTES];
        SystemRandom::new().fill(&mut nonce).map_err(|_| {
            tracing::error!("the system random source failed");
            DocError::Unavailable
        })?;
        let payload = Payload {
            msg: token.as_bytes(),
            aad: document_id.as_bytes(),
        };
        let sealed = self
            .cipher()
            .encrypt(Nonce::from_slice(&nonce), payload)
            .map_err(|_| DocError::Unavailable)?;
        Ok(Sealed {
            nonce: nonce.to_vec(),
            sealed,
        })
    }

    /// The token, or `None` when it was sealed under another key or for another document.
    pub(crate) fn open(&self, document_id: Uuid, nonce: &[u8], sealed: &[u8]) -> Option<String> {
        if nonce.len() != NONCE_BYTES {
            return None;
        }
        let payload = Payload {
            msg: sealed,
            aad: document_id.as_bytes(),
        };
        let token = self
            .cipher()
            .decrypt(Nonce::from_slice(nonce), payload)
            .ok()?;
        String::from_utf8(token).ok()
    }
}

impl fmt::Debug for LinkKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("LinkKey(<redacted>)")
    }
}

pub(crate) struct Sealed {
    pub nonce: Vec<u8>,
    pub sealed: Vec<u8>,
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";

    #[test]
    fn a_sealed_token_opens_with_its_key_and_document_only() {
        let key = LinkKey::from_bytes([7; 32]);
        let doc = Uuid::now_v7();
        let s = key.seal(doc, TOKEN).expect("seal");
        assert_eq!(s.nonce.len(), NONCE_BYTES);
        assert!(!s.sealed.windows(8).any(|w| TOKEN.as_bytes().starts_with(w)));
        assert_eq!(key.open(doc, &s.nonce, &s.sealed).as_deref(), Some(TOKEN));
        let other_key = LinkKey::from_bytes([8; 32]);
        assert_eq!(other_key.open(doc, &s.nonce, &s.sealed), None);
        assert_eq!(key.open(Uuid::now_v7(), &s.nonce, &s.sealed), None);
        assert_eq!(key.open(doc, &s.nonce[..11], &s.sealed), None);
        // A fresh nonce each time: the same token never seals to the same bytes.
        let again = key.seal(doc, TOKEN).expect("seal");
        assert_ne!(again.nonce, s.nonce);
    }

    #[test]
    fn keys_are_32_bytes_of_standard_base64() {
        let encoded = STANDARD.encode([1u8; 32]);
        assert_eq!(
            LinkKey::from_base64(&format!(" {encoded}\n")),
            Ok(LinkKey::from_bytes([1; 32]))
        );
        for bad in [
            "",
            "change-me",
            &STANDARD.encode([1u8; 16]),
            &STANDARD.encode([1u8; 33]),
        ] {
            assert_eq!(LinkKey::from_base64(bad), Err(InvalidLinkKey), "{bad:?}");
        }
        assert!(!format!("{:?}", LinkKey::from_bytes([1; 32])).contains('1'));
    }
}
