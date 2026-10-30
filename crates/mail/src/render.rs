//! The text of each mail, in Vietnamese. Plain text only: nothing a sharer types can become markup.
use crate::SharedNotice;

/// No `Debug`: holds names and the message.
#[derive(Clone, PartialEq, Eq)]
pub struct Rendered {
    pub subject: String,
    pub body: String,
}

/// Header values are one line of printable text, whatever PI or a sharer supplied.
pub(crate) fn one_line(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .trim()
        .to_owned()
}

/// A sharer's name in the Subject: long enough to recognize, too short to carry a pitch.
pub(crate) const SUBJECT_NAME_CHARS: usize = 60;

pub(crate) fn shortened(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_owned();
    }
    let mut out: String = s.chars().take(max - 1).collect();
    out.push('…');
    out
}

/// A sharer's message must not become a clickable link in the recipient's mail client: schemes
/// are written `hxxp`, and dots between letters `[.]`, as security teams quote suspicious URLs.
pub(crate) fn defanged(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len() + 8);
    let mut i = 0;
    while i < chars.len() {
        let rest: String = chars[i..chars.len().min(i + 8)].iter().collect();
        let lower = rest.to_ascii_lowercase();
        if lower.starts_with("http")
            && (lower[4..].starts_with("://") || lower[4..].starts_with("s://"))
        {
            out.push(chars[i]);
            out.push_str(if chars[i + 1].is_ascii_uppercase() {
                "XX"
            } else {
                "xx"
            });
            i += 3;
            continue;
        }
        let between_letters = chars[i] == '.'
            && i > 0
            && chars[i - 1].is_ascii_alphanumeric()
            && chars.get(i + 1).is_some_and(char::is_ascii_alphabetic);
        if between_letters {
            out.push_str("[.]");
        } else {
            out.push(chars[i]);
        }
        i += 1;
    }
    out
}

/// Keeps line breaks and tabs of a message, drops other control characters.
fn paragraph(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_control() || matches!(c, '\n' | '\t'))
        .collect::<String>()
        .trim()
        .to_owned()
}

/// The site's editor path of a document kind (`erpcons-office` `src/config/editors.ts`).
fn editor_slug(kind: &str) -> &'static str {
    match kind {
        "sheet" => "bang-tinh",
        "slide" => "trinh-chieu",
        "pdf" => "pdf",
        _ => "van-ban",
    }
}

fn role_vi(role: &str) -> &'static str {
    match role {
        "editor" => "chỉnh sửa",
        "commenter" => "nhận xét",
        _ => "xem",
    }
}

/// Link that opens the document in its editor on the site.
pub fn document_link(public_origin: &str, notice: &SharedNotice) -> String {
    format!(
        "{}/{}/?doc={}",
        public_origin.trim_end_matches('/'),
        editor_slug(&notice.kind),
        notice.document_id
    )
}

pub fn render_shared(notice: &SharedNotice, public_origin: &str) -> Rendered {
    let sharer = Some(one_line(&notice.sharer_name))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "Một người dùng ERPCons Office".to_owned());
    let email = notice
        .sharer_email
        .as_deref()
        .map(one_line)
        .filter(|e| !e.is_empty());
    let title = notice
        .title
        .as_deref()
        .map(one_line)
        .filter(|t| !t.is_empty());
    let short = shortened(&sharer, SUBJECT_NAME_CHARS);
    let subject = match &title {
        Some(t) => format!("{short} đã chia sẻ \"{t}\" với bạn"),
        None => format!("{short} đã chia sẻ một tài liệu với bạn"),
    };
    let what = match &title {
        Some(t) => format!("tài liệu \"{t}\""),
        None => "một tài liệu".to_owned(),
    };
    let who = match &email {
        Some(e) => format!("{sharer} ({e})"),
        None => sharer.clone(),
    };
    let mut body = format!(
        "Xin chào,\n\n{who} đã chia sẻ {what} với bạn trên ERPCons Office, với quyền {}.\n",
        role_vi(&notice.role)
    );
    if let Some(message) = notice
        .message
        .as_deref()
        .map(paragraph)
        .filter(|m| !m.is_empty())
    {
        body.push_str(&format!(
            "\nLời nhắn do {who} tự viết (không phải của ERPCons Office):\n{}\n",
            defanged(&message)
        ));
    }
    body.push_str(&format!(
        "\nMở tài liệu: {}\n\nHãy đăng nhập bằng tài khoản PI ID có địa chỉ email này. Tài liệu cũng có trong \
         mục \"Được chia sẻ với tôi\".\n\n-- \nERPCons Office\n",
        document_link(public_origin, notice)
    ));
    Rendered { subject, body }
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::*;

    fn notice(title: Option<&str>, message: Option<&str>) -> SharedNotice {
        SharedNotice {
            document_id: Uuid::nil(),
            kind: "sheet".into(),
            role: "editor".into(),
            sharer_name: "Trần\r\nBcc: x@y.z Bình".into(),
            sharer_email: Some("binh@erpcons.vn".into()),
            message: message.map(str::to_owned),
            title: title.map(str::to_owned),
        }
    }

    #[test]
    fn without_a_title_the_mail_names_none() {
        let r = render_shared(
            &notice(None, Some("Nhờ anh xem\r\ngiúp")),
            "https://office.erpcons.vn/",
        );
        assert!(r.subject.contains("một tài liệu"));
        assert!(!r.subject.contains('\n') && !r.subject.contains('\r'));
        assert!(r.body.contains("quyền chỉnh sửa"));
        assert!(r.body.contains("Nhờ anh xem\ngiúp"));
        assert!(r.body.contains(
            "https://office.erpcons.vn/bang-tinh/?doc=00000000-0000-0000-0000-000000000000"
        ));
    }

    #[test]
    fn a_title_is_used_when_given() {
        let r = render_shared(
            &notice(Some("Báo cáo Q3"), None),
            "https://office.erpcons.vn",
        );
        assert!(r.subject.contains("\"Báo cáo Q3\""));
        assert!(r.body.contains("tài liệu \"Báo cáo Q3\""));
        assert!(!r.body.contains("Lời nhắn"));
    }

    #[test]
    fn the_message_is_signed_by_its_sender_and_carries_no_live_links() {
        let r = render_shared(
            &notice(
                None,
                Some(
                    "Đăng nhập lại tại https://evil.example/login hoặc HTTP://x.co, giá 3.5 triệu",
                ),
            ),
            "https://office.erpcons.vn",
        );
        assert!(
            r.body.contains("(binh@erpcons.vn) đã chia sẻ"),
            "{}",
            r.body
        );
        assert!(r.body.contains("Lời nhắn do"), "{}", r.body);
        assert!(
            r.body.contains("hxxps://evil[.]example/login"),
            "{}",
            r.body
        );
        assert!(r.body.contains("HXXP://x[.]co"), "{}", r.body);
        assert!(r.body.contains("3.5 triệu"), "{}", r.body);
        assert!(!r.body.contains("https://evil"));
        // The document link of the mail itself stays live.
        assert!(r.body.contains("https://office.erpcons.vn/bang-tinh/?doc="));
    }

    #[test]
    fn long_names_are_cut_in_the_subject_only() {
        let mut n = notice(None, None);
        n.sharer_name = format!("Khuyến mãi {}", "x".repeat(200));
        let r = render_shared(&n, "https://office.erpcons.vn");
        let name = r.subject.split(" đã chia sẻ").next().expect("name");
        assert_eq!(name.chars().count(), SUBJECT_NAME_CHARS);
        assert!(r.body.contains(&"x".repeat(200)));
    }
}
