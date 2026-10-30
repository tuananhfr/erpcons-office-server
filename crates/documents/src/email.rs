use std::fmt;

use unicode_normalization::UnicodeNormalization;

const MAX_CHARS: usize = 254;
const MAX_LOCAL_CHARS: usize = 64;

/// An email address as stored and compared: trimmed, NFC, lowercase (plan v2 §2). Invitations and
/// PI's verified emails meet only in this form.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct Email(String);

// Personal data: `{:?}` must not print it.
impl fmt::Debug for Email {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Email(<redacted>)")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("invalid email address")]
pub struct InvalidEmail;

impl Email {
    /// One `@` with a non-empty part on each side and a dot inside the domain; no spaces, control
    /// characters or the separators a list of addresses would use. Deliverability is SMTP's business.
    pub fn parse(raw: &str) -> Result<Self, InvalidEmail> {
        let norm: String = raw.trim().nfc().collect::<String>().to_lowercase();
        let (local, domain) = norm.split_once('@').ok_or(InvalidEmail)?;
        let bad_char = |c: char| c.is_whitespace() || c.is_control() || "@,;<>()[]\\\"".contains(c);
        let domain_ok = !domain.starts_with('.')
            && !domain.ends_with('.')
            && domain.contains('.')
            && !domain.contains("..");
        if norm.chars().count() > MAX_CHARS
            || local.is_empty()
            || local.chars().count() > MAX_LOCAL_CHARS
            || !domain_ok
            || local.chars().any(bad_char)
            || domain.chars().any(bad_char)
        {
            return Err(InvalidEmail);
        }
        Ok(Self(norm))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_are_trimmed_nfc_and_lowercase() {
        let decomposed = "  Bi\u{0301}nh@Example.VN ";
        assert_eq!(
            Email::parse(decomposed).expect("valid").as_str(),
            "bính@example.vn"
        );
        assert_eq!(
            Email::parse("AN@x.vn").expect("valid"),
            Email::parse("an@X.VN").expect("valid")
        );
    }

    #[test]
    fn malformed_addresses_are_refused() {
        let long = format!("{}@x.vn", "a".repeat(65));
        for bad in [
            "",
            "an",
            "@x.vn",
            "an@",
            "an@x",
            "an@.x.vn",
            "an@x.vn.",
            "a n@x.vn",
            "an@x..vn",
            "an@b@x.vn",
            "an,b@x.vn",
            "<an@x.vn>",
            "an@x.vn\r\nBcc: b@x.vn",
            &long,
        ] {
            assert_eq!(Email::parse(bad), Err(InvalidEmail), "{bad:?}");
        }
    }

    #[test]
    fn debug_hides_the_address() {
        assert!(!format!("{:?}", Email::parse("an@x.vn").expect("valid")).contains("an@"));
    }
}
