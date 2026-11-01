//! The two comment mails, in Vietnamese. They say where to look, never what anyone wrote.
use crate::{
    CommentNotice, Rendered,
    render::{SUBJECT_NAME_CHARS, editor_slug, one_line, shortened},
};

/// Link that opens the document in its editor with the thread shown.
pub fn comment_link(public_origin: &str, notice: &CommentNotice) -> String {
    format!(
        "{}/{}/?doc={}&comment={}",
        public_origin.trim_end_matches('/'),
        editor_slug(&notice.kind),
        notice.document_id,
        notice.thread_id
    )
}

fn title_of(notice: &CommentNotice) -> Option<String> {
    notice
        .title
        .as_deref()
        .map(one_line)
        .filter(|t| !t.is_empty())
}

const SIGNATURE: &str = "\n-- \nERPCons Office\n";

/// Someone replied in a thread the recipient wrote in.
pub fn render_comment_reply(notice: &CommentNotice, public_origin: &str) -> Rendered {
    let replier = notice
        .replier_name
        .as_deref()
        .map(one_line)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "Một người dùng ERPCons Office".to_owned());
    let title = title_of(notice);
    let short = shortened(&replier, SUBJECT_NAME_CHARS);
    let subject = match &title {
        Some(t) => format!("{short} đã trả lời nhận xét trên \"{t}\""),
        None => format!("{short} đã trả lời một nhận xét của bạn"),
    };
    let what = match &title {
        Some(t) => format!("tài liệu \"{t}\""),
        None => "một tài liệu".to_owned(),
    };
    let body = format!(
        "Xin chào,\n\n{replier} đã trả lời trong một chuỗi nhận xét bạn tham gia, trên {what} trong \
         ERPCons Office.\n\nXem nhận xét: {}\n{SIGNATURE}",
        comment_link(public_origin, notice)
    );
    Rendered { subject, body }
}

/// `count` new threads on the recipient's document.
pub fn render_comment_threads(notice: &CommentNotice, count: u32, public_origin: &str) -> Rendered {
    let title = title_of(notice);
    let subject = match &title {
        Some(t) => format!("Có nhận xét mới trên \"{t}\""),
        None => "Có nhận xét mới trên tài liệu của bạn".to_owned(),
    };
    let what = match &title {
        Some(t) => format!("tài liệu \"{t}\" của bạn"),
        None => "một tài liệu của bạn".to_owned(),
    };
    let body = format!(
        "Xin chào,\n\nCó {count} nhận xét mới trên {what} trong ERPCons Office.\n\nXem nhận xét: {}\n{SIGNATURE}",
        comment_link(public_origin, notice)
    );
    Rendered { subject, body }
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::*;

    fn notice(title: Option<&str>) -> CommentNotice {
        CommentNotice {
            document_id: Uuid::nil(),
            kind: "slide".into(),
            thread_id: Uuid::from_u128(7),
            recipient_id: "usr_a".into(),
            replier_name: Some("Cúc\r\nBcc: x@y.z".into()),
            since: None,
            title: title.map(str::to_owned),
        }
    }

    #[test]
    fn the_link_opens_the_thread_in_its_editor() {
        let r = render_comment_reply(&notice(None), "https://office.erpcons.vn/");
        assert!(r.body.contains(
            "https://office.erpcons.vn/trinh-chieu/?doc=00000000-0000-0000-0000-000000000000\
             &comment=00000000-0000-0000-0000-000000000007"
        ));
        assert!(!r.subject.contains('\n') && !r.subject.contains('\r'));
        assert!(r.subject.contains("một nhận xét"));
    }

    #[test]
    fn titles_appear_only_when_given() {
        let r = render_comment_threads(&notice(Some("Kế hoạch")), 3, "https://o.test");
        assert!(r.subject.contains("\"Kế hoạch\""));
        assert!(r.body.contains("Có 3 nhận xét mới"));
        let r = render_comment_threads(&notice(None), 1, "https://o.test");
        assert!(!r.subject.contains('"') && !r.body.contains("Kế hoạch"));
    }
}
