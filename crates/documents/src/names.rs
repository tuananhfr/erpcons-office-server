use std::fmt;

use unicode_normalization::UnicodeNormalization;

const MAX_CHARS: usize = 255;

/// A folder name or document title as stored: trimmed and NFC, so equal-looking names are equal.
#[derive(Clone, PartialEq, Eq)]
pub struct Name {
    display: String,
}

// Names are personal data: `{:?}` must not print them.
impl fmt::Debug for Name {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Name(<redacted>)")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("invalid name")]
pub struct InvalidName;

impl Name {
    /// Applies the contract's `Name` rules: 1–255 characters after trimming, no control or bidi
    /// characters, no `/` or `\`.
    pub fn parse(raw: &str) -> Result<Self, InvalidName> {
        let display: String = raw.trim().nfc().collect();
        let len = display.chars().count();
        if len == 0 || len > MAX_CHARS || display.chars().any(forbidden) {
            return Err(InvalidName);
        }
        Ok(Self { display })
    }

    pub fn as_str(&self) -> &str {
        &self.display
    }

    /// Uniqueness key of folder names (owner decision 27/10): case-insensitive on the NFC form.
    pub fn key(&self) -> String {
        self.display.to_lowercase()
    }
}

/// Bidi controls can make "\u{202e}fdp.exe" read as "exe.pdf" (same set as the site's intake).
fn forbidden(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '/' | '\\'
                | '\u{061c}'
                | '\u{200e}'
                | '\u{200f}'
                | '\u{202a}'..='\u{202e}'
                | '\u{2066}'..='\u{2069}'
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(s: &str) -> String {
        Name::parse(s).expect("valid").key()
    }

    #[test]
    fn trims_and_normalizes_to_nfc() {
        let decomposed = "Ho\u{031b}\u{0323}p \u{0111}o\u{0302}\u{0300}ng";
        let name = Name::parse(&format!("  {decomposed}\t")).expect("valid");
        assert_eq!(name.as_str(), "Hợp đồng");
    }

    #[test]
    fn keys_ignore_case_and_normalization() {
        let decomposed = "ho\u{031b}\u{0323}p \u{0111}o\u{0302}\u{0300}ng";
        assert_eq!(key("Hợp đồng"), key("hợp đồng"));
        assert_eq!(key("HỢP ĐỒNG"), key(decomposed));
        assert_ne!(key("Hợp đồng"), key("Hop dong"));
    }

    #[test]
    fn rejects_what_the_contract_forbids() {
        for bad in [
            "",
            "   ",
            "a/b",
            "a\\b",
            "a\nb",
            "a\u{0000}b",
            "\u{202e}fdp.exe",
            "a\u{2067}b",
            "a\u{200f}",
            "a\u{7f}",
        ] {
            assert_eq!(Name::parse(bad), Err(InvalidName), "{bad:?}");
        }
        assert!(Name::parse(&"ắ".repeat(255)).is_ok());
        assert_eq!(Name::parse(&"a".repeat(256)), Err(InvalidName));
    }
}
