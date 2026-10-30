//! Public view link tokens: 256 random bits. They are looked up by sha256, and kept otherwise only
//! sealed under the server's key (`link_cipher`), so a database leak alone opens no document.
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use ring::rand::{SecureRandom, SystemRandom};
use sha2::{Digest, Sha256};

use crate::DocError;

const TOKEN_BYTES: usize = 32;
/// base64url of 32 bytes, without padding.
pub(crate) const TOKEN_CHARS: usize = 43;

pub(crate) fn generate() -> Result<String, DocError> {
    let mut bytes = [0u8; TOKEN_BYTES];
    SystemRandom::new().fill(&mut bytes).map_err(|_| {
        tracing::error!("the system random source failed");
        DocError::Unavailable
    })?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

/// The stored form of a token; `None` for anything that cannot be a token, so it is refused
/// without a database query.
pub(crate) fn hash(token: &str) -> Option<String> {
    let well_formed = token.len() == TOKEN_CHARS
        && token
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    well_formed.then(|| hex::encode(Sha256::digest(token.as_bytes())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_fresh_well_formed_and_hash_to_hex() {
        let a = generate().expect("token");
        let b = generate().expect("token");
        assert_ne!(a, b);
        assert_eq!(a.len(), TOKEN_CHARS);
        let h = hash(&a).expect("well formed");
        assert_eq!(h.len(), 64);
        assert!(!h.contains(&a));
    }

    #[test]
    fn malformed_tokens_never_reach_the_database() {
        for bad in [
            "",
            "short",
            &"a".repeat(44),
            &format!("{}=", "a".repeat(42)),
            &"é".repeat(22),
        ] {
            assert_eq!(hash(bad), None, "{bad:?}");
        }
    }
}
