use std::{fmt, str::FromStr};

use sha2::{Digest, Sha256};

/// A SHA-256 digest, written as 64 lowercase hex characters.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Sha256Digest([u8; 32]);

impl Sha256Digest {
    pub fn of(bytes: &[u8]) -> Self {
        Self(Sha256::digest(bytes).into())
    }

    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }
}

impl fmt::Display for Sha256Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

impl fmt::Debug for Sha256Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Sha256Digest({self})")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("not a lowercase hex sha256")]
pub struct InvalidDigest;

impl FromStr for Sha256Digest {
    type Err = InvalidDigest;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        // Uppercase is refused so that one digest has exactly one key and one path.
        if s.len() != 64 || !s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
            return Err(InvalidDigest);
        }
        let mut out = [0u8; 32];
        hex::decode_to_slice(s, &mut out).map_err(|_| InvalidDigest)?;
        Ok(Self(out))
    }
}

/// Where an adapter keeps an object. Opaque to callers; each adapter defines its format.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ObjectKey(String);

impl ObjectKey {
    pub fn new(key: impl Into<String>) -> Self {
        Self(key.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ObjectKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digest_round_trips_through_hex() {
        let d = Sha256Digest::of(b"abc");
        assert_eq!(
            d.to_hex(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(d.to_hex().parse::<Sha256Digest>(), Ok(d));
    }

    #[test]
    fn digest_parse_rejects_uppercase_and_bad_length() {
        let upper = Sha256Digest::of(b"abc").to_hex().to_uppercase();
        assert_eq!(upper.parse::<Sha256Digest>(), Err(InvalidDigest));
        assert_eq!("abc".parse::<Sha256Digest>(), Err(InvalidDigest));
        assert_eq!(
            "../".repeat(22)[..64].parse::<Sha256Digest>(),
            Err(InvalidDigest)
        );
    }
}
