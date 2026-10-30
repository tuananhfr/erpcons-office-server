//! The password hash PI adopts as the PI ID password (D515). PI takes `$2y$` bcrypt or argon2 in PHP's
//! format; `$2y$` is the one every PHP build verifies, argon2 depends on how PHP was compiled.
use bcrypt::Version;
use ring::rand::{SecureRandom, SystemRandom};

/// PHP's own default is 10; 12 costs about a quarter second, run off the async threads.
pub const BCRYPT_COST: u32 = 12;

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("the password could not be hashed")]
pub struct PasswordHashError;

/// `$2y$12$…` of `password`, with a salt from the system random source. The password is at most
/// 72 bytes (checked with the other rules), so bcrypt never cuts it.
pub async fn hash_password(password: String) -> Result<String, PasswordHashError> {
    let mut salt = [0u8; 16];
    SystemRandom::new().fill(&mut salt).map_err(|_| {
        tracing::error!("the system random source failed");
        PasswordHashError
    })?;
    tokio::task::spawn_blocking(move || {
        bcrypt::non_truncating_hash_with_salt(password.as_bytes(), BCRYPT_COST, salt)
            .map(|parts| parts.format_for_version(Version::TwoY))
            .map_err(|_| PasswordHashError)
    })
    .await
    .map_err(|_| PasswordHashError)?
}

/// PI's `Passwords::portableHash` (`^\$(2y|argon2id|argon2i)\$[A-Za-z0-9./$=,+-]{20,250}$`): a hash
/// PI refuses would leave the new PI ID account without a password.
pub fn portable_hash(hash: &str) -> bool {
    let Some(rest) = ["$2y$", "$argon2id$", "$argon2i$"]
        .iter()
        .find_map(|prefix| hash.strip_prefix(prefix))
    else {
        return false;
    };
    (20..=250).contains(&rest.len())
        && rest
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"./$=,+-".contains(&b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn hashes_are_2y_bcrypt_that_pi_accepts_and_verify() {
        let hash = hash_password("mật khẩu 1".to_owned()).await.expect("hash");
        assert!(hash.starts_with("$2y$12$"), "{hash}");
        assert_eq!(hash.len(), 60);
        assert!(portable_hash(&hash));
        assert!(bcrypt::verify("mật khẩu 1", &hash).expect("verify"));
        assert!(!bcrypt::verify("mật khẩu 2", &hash).expect("verify"));
        let again = hash_password("mật khẩu 1".to_owned()).await.expect("hash");
        assert_ne!(again, hash, "a fresh salt each time");
    }

    #[test]
    fn portable_hash_follows_pi() {
        assert!(portable_hash(
            "$argon2id$v=19$m=65536,t=4,p=1$c2FsdHNhbHQ$aGFzaGhhc2hoYXNoaGFzaA"
        ));
        for bad in [
            "",
            "$2b$12$abcdefghijklmnopqrstuv",
            "$2y$short",
            "$S$Dabcdefghijklmnopqrstuvwxyz",
            "$2y$12$abcdefghijklmnop qrstuvwxyz",
        ] {
            assert!(!portable_hash(bad), "{bad}");
        }
    }
}
