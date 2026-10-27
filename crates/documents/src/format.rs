/// What a document is, as clients see it (`Kind` in the contract).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Word,
    Sheet,
    Slide,
    Pdf,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Word => "word",
            Self::Sheet => "sheet",
            Self::Slide => "slide",
            Self::Pdf => "pdf",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "word" => Self::Word,
            "sheet" => Self::Sheet,
            "slide" => Self::Slide,
            "pdf" => Self::Pdf,
            _ => return None,
        })
    }
}

/// The file formats stored in phase 1. Each maps to exactly one kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Docx,
    Xlsx,
    Pptx,
    Pdf,
}

impl Format {
    pub const ALL: [Format; 4] = [Self::Docx, Self::Xlsx, Self::Pptx, Self::Pdf];

    pub fn extension(self) -> &'static str {
        match self {
            Self::Docx => "docx",
            Self::Xlsx => "xlsx",
            Self::Pptx => "pptx",
            Self::Pdf => "pdf",
        }
    }

    pub fn mime(self) -> &'static str {
        match self {
            Self::Docx => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
            Self::Xlsx => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
            Self::Pptx => {
                "application/vnd.openxmlformats-officedocument.presentationml.presentation"
            }
            Self::Pdf => "application/pdf",
        }
    }

    pub fn kind(self) -> Kind {
        match self {
            Self::Docx => Kind::Word,
            Self::Xlsx => Kind::Sheet,
            Self::Pptx => Kind::Slide,
            Self::Pdf => Kind::Pdf,
        }
    }

    pub fn for_kind(kind: Kind) -> Self {
        match kind {
            Kind::Word => Self::Docx,
            Kind::Sheet => Self::Xlsx,
            Kind::Slide => Self::Pptx,
            Kind::Pdf => Self::Pdf,
        }
    }

    /// Case-insensitive, as the site's `extensionOf`.
    pub fn from_extension(ext: &str) -> Option<Self> {
        let ext = ext.to_ascii_lowercase();
        Self::ALL.into_iter().find(|f| f.extension() == ext)
    }

    /// The format of a file name, by its last extension.
    pub fn from_file_name(name: &str) -> Option<Self> {
        let (_, ext) = name.rsplit_once('.')?;
        Self::from_extension(ext)
    }

    pub fn from_mime(mime: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|f| f.mime() == mime)
    }

    /// Office Open XML formats are ZIP packages.
    pub fn is_ooxml(self) -> bool {
        !matches!(self, Self::Pdf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_format_round_trips() {
        for format in Format::ALL {
            assert_eq!(Format::from_extension(format.extension()), Some(format));
            assert_eq!(Format::from_mime(format.mime()), Some(format));
            assert_eq!(Format::for_kind(format.kind()), format);
            assert_eq!(Kind::parse(format.kind().as_str()), Some(format.kind()));
        }
    }

    #[test]
    fn file_names_use_the_last_extension() {
        assert_eq!(Format::from_file_name("Báo cáo.DOCX"), Some(Format::Docx));
        assert_eq!(Format::from_file_name("a.pdf.xlsx"), Some(Format::Xlsx));
        assert_eq!(Format::from_file_name("noext"), None);
        assert_eq!(Format::from_file_name("old.doc"), None);
        assert_eq!(Format::from_file_name("x.docm"), None);
    }
}
