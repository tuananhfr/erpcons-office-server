//! Verification tokens: 256 random bits, stored only as sha256 and sent only in the mail.
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use ring::rand::{SecureRandom, SystemRandom};
use sha2::{Digest, Sha256};

/// base64url of 32 bytes, without padding.
const TOKEN_CHARS: usize = 43;

pub(crate) fn generate() -> Option<String> {
    let mut bytes = [0u8; 32];
    if SystemRandom::new().fill(&mut bytes).is_err() {
        tracing::error!("the system random source failed");
        return None;
    }
    Some(URL_SAFE_NO_PAD.encode(bytes))
}

/// The stored form of a token; `None` for anything that cannot be one, refused without a query.
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
    fn tokens_are_fresh_and_hash_to_hex() {
        let a = generate().expect("token");
        assert_ne!(a, generate().expect("token"));
        assert_eq!(a.len(), TOKEN_CHARS);
        assert_eq!(hash(&a).expect("well formed").len(), 64);
        for bad in ["", "short", &"a".repeat(44), &"é".repeat(22)] {
            assert_eq!(hash(bad), None, "{bad:?}");
        }
    }
}
