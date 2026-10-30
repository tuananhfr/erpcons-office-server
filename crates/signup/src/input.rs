//! The rules of a sign-up form, shared with the site's `src/lib/signup/rules.ts`.
use office_documents::Email;
use serde::Deserialize;
use unicode_normalization::UnicodeNormalization;

use crate::{Field, SignupError};

pub(crate) const NAME_MAX_CHARS: usize = 100;
pub(crate) const PASSWORD_MIN_CHARS: usize = 8;
/// bcrypt reads only the first 72 bytes; a longer password would sign in with its prefix alone.
pub(crate) const PASSWORD_MAX_BYTES: usize = 72;

/// The body of `POST /signup`. `website` is a field people never see: a bot that fills it is
/// answered as usual and nothing happens. No `Debug`: personal data and a password.
#[derive(Clone, Default, Deserialize)]
pub struct SignupRequest {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub email: String,
    #[serde(default)]
    pub password: String,
    #[serde(default)]
    pub website: Option<String>,
}

impl SignupRequest {
    pub(crate) fn is_bot(&self) -> bool {
        self.website
            .as_deref()
            .is_some_and(|w| !w.trim().is_empty())
    }
}

/// A sign-up that passed every rule. No `Debug`.
pub(crate) struct ValidSignup {
    pub email: Email,
    pub name: String,
    pub password: String,
}

/// Trimmed and NFC; 1–100 characters without control characters.
fn name(raw: &str) -> Result<String, SignupError> {
    let name: String = raw.trim().nfc().collect();
    let count = name.chars().count();
    if count == 0 || count > NAME_MAX_CHARS || name.chars().any(char::is_control) {
        return Err(SignupError::Invalid(Field::Name));
    }
    Ok(name)
}

/// PI checks addresses with PHP's `FILTER_VALIDATE_EMAIL`, which refuses non-ASCII ones.
fn email(raw: &str) -> Result<Email, SignupError> {
    Email::parse(raw)
        .ok()
        .filter(|e| e.as_str().is_ascii())
        .ok_or(SignupError::Invalid(Field::Email))
}

/// PI takes the hash as is and applies no policy of its own, so every rule lives here.
fn password(raw: &str, email: &Email) -> Result<String, SignupError> {
    if raw.chars().count() < PASSWORD_MIN_CHARS
        || raw.len() > PASSWORD_MAX_BYTES
        || raw.trim().to_lowercase() == email.as_str()
    {
        return Err(SignupError::Invalid(Field::Password));
    }
    Ok(raw.to_owned())
}

pub(crate) fn validate(request: &SignupRequest) -> Result<ValidSignup, SignupError> {
    let name = name(&request.name)?;
    let email = email(&request.email)?;
    let password = password(&request.password, &email)?;
    Ok(ValidSignup {
        email,
        name,
        password,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(name: &str, email: &str, password: &str) -> SignupRequest {
        SignupRequest {
            name: name.to_owned(),
            email: email.to_owned(),
            password: password.to_owned(),
            website: None,
        }
    }

    #[test]
    fn a_good_sign_up_is_normalized() {
        let v = validate(&request(
            "  Nguyễn Văn An ",
            " An@Example.VN ",
            "mật khẩu 1",
        ))
        .expect("valid");
        assert_eq!(v.name, "Nguyễn Văn An");
        assert_eq!(v.email.as_str(), "an@example.vn");
        assert_eq!(v.password, "mật khẩu 1");
    }

    #[test]
    fn each_rule_names_its_field() {
        let long_name = "a".repeat(NAME_MAX_CHARS + 1);
        let cases = [
            (request("", "an@x.vn", "12345678"), Field::Name),
            (request("   ", "an@x.vn", "12345678"), Field::Name),
            (request(&long_name, "an@x.vn", "12345678"), Field::Name),
            (request("An\u{7}", "an@x.vn", "12345678"), Field::Name),
            (request("An", "an", "12345678"), Field::Email),
            (request("An", "bính@x.vn", "12345678"), Field::Email),
            (request("An", "an@x.vn", "1234567"), Field::Password),
            (request("An", "an@x.vn", &"đ".repeat(37)), Field::Password),
            (
                request("An", "an.nguyen@x.vn", "An.Nguyen@X.vn"),
                Field::Password,
            ),
        ];
        for (input, field) in cases {
            assert_eq!(validate(&input).err(), Some(SignupError::Invalid(field)));
        }
        assert!(validate(&request("A", "an@x.vn", &"a".repeat(72))).is_ok());
        assert!(validate(&request(&"a".repeat(NAME_MAX_CHARS), "an@x.vn", "12345678")).is_ok());
    }

    #[test]
    fn the_hidden_field_marks_a_bot() {
        let mut r = request("An", "an@x.vn", "12345678");
        assert!(!r.is_bot());
        r.website = Some("  ".to_owned());
        assert!(!r.is_bot());
        r.website = Some("http://spam.example".to_owned());
        assert!(r.is_bot());
    }
}
