use crate::Format;

/// The empty file a blank document starts from. How each was made: `templates/README.md`.
pub fn blank(format: Format) -> &'static [u8] {
    match format {
        Format::Docx => include_bytes!("../templates/blank.docx"),
        Format::Xlsx => include_bytes!("../templates/blank.xlsx"),
        Format::Pptx => include_bytes!("../templates/blank.pptx"),
        Format::Pdf => include_bytes!("../templates/blank.pdf"),
    }
}

/// Title of a blank document created without one.
pub fn default_title(format: Format) -> &'static str {
    match format {
        Format::Docx => "Văn bản không tên",
        Format::Xlsx => "Bảng tính không tên",
        Format::Pptx => "Trình chiếu không tên",
        Format::Pdf => "PDF không tên",
    }
}
