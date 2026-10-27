//! Port of the site's signature table (`erpcons-office/src/lib/files/intake-contract.ts`,
//! `SIGNATURES` and `SIGNATURE_RULES`) for the phase-1 formats. `tests/intake_vectors.rs` checks it
//! against that file, so the two cannot drift silently.
use super::Rejection;
use crate::Format;

/// How many leading bytes the site reads (`INTAKE_LIMITS.headBytes`).
pub const HEAD_BYTES: usize = 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Container {
    Zip,
    Ole,
    Rtf,
    Pdf,
}

impl Container {
    pub const ALL: [Container; 4] = [Self::Zip, Self::Ole, Self::Rtf, Self::Pdf];

    pub fn name(self) -> &'static str {
        match self {
            Self::Zip => "zip",
            Self::Ole => "ole",
            Self::Rtf => "rtf",
            Self::Pdf => "pdf",
        }
    }

    pub fn magic(self) -> &'static [u8] {
        match self {
            Self::Zip => &[0x50, 0x4b, 0x03, 0x04],
            Self::Ole => &[0xd0, 0xcf, 0x11, 0xe0, 0xa1, 0xb1, 0x1a, 0xe1],
            Self::Rtf => &[0x7b, 0x5c, 0x72, 0x74, 0x66],
            Self::Pdf => &[0x25, 0x50, 0x44, 0x46, 0x2d],
        }
    }

    /// Latest offset the magic may start at (a PDF may have a preamble).
    pub fn within(self) -> usize {
        match self {
            Self::Pdf => HEAD_BYTES - self.magic().len(),
            _ => 0,
        }
    }

    /// `hasSignature` of the site.
    pub fn matches(self, head: &[u8]) -> bool {
        let magic = self.magic();
        (0..=self.within())
            .take_while(|start| start + magic.len() <= head.len())
            .any(|start| &head[start..start + magic.len()] == magic)
    }
}

/// What the site accepts for each format (`SIGNATURE_RULES`).
pub fn site_rules(format: Format) -> &'static [Container] {
    match format {
        Format::Docx => &[Container::Zip, Container::Ole, Container::Rtf],
        Format::Xlsx => &[Container::Zip, Container::Ole],
        Format::Pptx => &[Container::Zip],
        Format::Pdf => &[Container::Pdf],
    }
}

/// What the server stores: OOXML must be a ZIP (its central directory is checked for bombs);
/// the encrypted (OLE) and RTF variants the site lets editors open are not stored in phase 1.
pub fn server_rules(format: Format) -> &'static [Container] {
    match format {
        Format::Docx | Format::Xlsx | Format::Pptx => &[Container::Zip],
        Format::Pdf => &[Container::Pdf],
    }
}

/// Checks the first [`HEAD_BYTES`] (or the whole file when shorter) against `format`.
pub fn check_head(format: Format, head: &[u8]) -> Result<(), Rejection> {
    if server_rules(format).iter().any(|c| c.matches(head)) {
        return Ok(());
    }
    // A real file of a variant phase 1 does not store, or (as the site's `encryptedPresentation`)
    // an OLE file named as a presentation: unsupported rather than a forged signature.
    let other_variant = site_rules(format).iter().any(|c| c.matches(head))
        || (format.is_ooxml() && Container::Ole.matches(head));
    if !head.is_empty() && other_variant {
        Err(Rejection::UnsupportedType)
    } else {
        Err(Rejection::InvalidSignature)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pdf_marker_may_start_up_to_the_end_of_the_head() {
        let mut head = vec![b' '; HEAD_BYTES];
        head[HEAD_BYTES - 5..].copy_from_slice(b"%PDF-");
        assert!(Container::Pdf.matches(&head));
        let mut late = vec![b' '; HEAD_BYTES + 1];
        late[HEAD_BYTES - 4..].copy_from_slice(b"%PDF-");
        assert!(!Container::Pdf.matches(&late));
    }

    #[test]
    fn server_rules_are_a_subset_of_the_site_rules() {
        for format in Format::ALL {
            for c in server_rules(format) {
                assert!(site_rules(format).contains(c), "{format:?} {c:?}");
            }
        }
    }

    #[test]
    fn verdicts() {
        assert_eq!(check_head(Format::Docx, b"PK\x03\x04rest"), Ok(()));
        assert_eq!(check_head(Format::Pdf, b"%PDF-1.7"), Ok(()));
        assert_eq!(
            check_head(Format::Docx, b""),
            Err(Rejection::InvalidSignature)
        );
        assert_eq!(
            check_head(Format::Docx, b"%PDF-1.7"),
            Err(Rejection::InvalidSignature)
        );
        assert_eq!(
            check_head(Format::Docx, b"{\\rtf1"),
            Err(Rejection::UnsupportedType)
        );
        let ole = Container::Ole.magic();
        assert_eq!(
            check_head(Format::Pptx, ole),
            Err(Rejection::UnsupportedType)
        );
        assert_eq!(
            check_head(Format::Pdf, ole),
            Err(Rejection::InvalidSignature)
        );
    }
}
