//! The two sign-up mails, in Vietnamese plain text. The name is whatever the person typed, so it is
//! kept to one short line and can carry no live link.
use crate::{
    Rendered,
    render::{SUBJECT_NAME_CHARS, defanged, one_line, shortened},
};

/// Hours a verification link stays valid; the text says so.
pub const VERIFY_TTL_HOURS: i64 = 48;

/// The site's verification page; the token rides in the fragment, which no request or log carries.
pub fn verify_link(public_origin: &str, token: &str) -> String {
    format!(
        "{}/dang-ky/xac-minh/#token={token}",
        public_origin.trim_end_matches('/')
    )
}

pub fn render_signup_verify(display_name: &str, token: &str, public_origin: &str) -> Rendered {
    let name = defanged(&shortened(&one_line(display_name), SUBJECT_NAME_CHARS));
    let greeting = if name.is_empty() {
        "Xin chào,".to_owned()
    } else {
        format!("Xin chào {name},")
    };
    let body = format!(
        "{greeting}\n\nĐịa chỉ email này vừa được dùng để đăng ký tài khoản ERPCons Office miễn phí. Mở liên kết \
         dưới đây trong vòng {VERIFY_TTL_HOURS} giờ để xác minh email; tài khoản PI ID của bạn sẽ được tạo ngay sau \
         đó:\n\n{}\n\nSau khi xác minh, hãy đăng nhập ERPCons Office bằng email này và mật khẩu bạn vừa đặt.\n\n\
         Nếu bạn không đăng ký, hãy bỏ qua thư này: không có tài khoản nào được tạo.\n\n-- \nERPCons Office\n",
        verify_link(public_origin, token)
    );
    Rendered {
        subject: "Xác minh email để tạo tài khoản ERPCons Office".to_owned(),
        body,
    }
}

pub fn render_signup_existing(public_origin: &str) -> Rendered {
    let body = format!(
        "Xin chào,\n\nEmail của bạn đã được xác minh. Địa chỉ này đã có tài khoản PI ID từ trước, nên mật khẩu \
         vừa đặt lúc đăng ký không thay mật khẩu PI ID hiện có.\n\nHãy đăng nhập ERPCons Office tại {}/ bằng mật \
         khẩu PI ID bạn đang dùng. Nếu quên mật khẩu, chọn \"Quên mật khẩu\" ở trang đăng nhập PI ID.\n\n-- \n\
         ERPCons Office\n",
        public_origin.trim_end_matches('/')
    );
    Rendered {
        subject: "Email của bạn đã có tài khoản PI ID".to_owned(),
        body,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN: &str = "AbCdEfGhIjKlMnOpQrStUvWxYz0123456789-_AbCd";

    #[test]
    fn the_link_carries_the_token_in_the_fragment() {
        let r = render_signup_verify("Nguyễn Văn An", TOKEN, "https://office.erpcons.vn/");
        assert!(r.body.starts_with("Xin chào Nguyễn Văn An,"));
        assert!(
            r.body.contains(&format!(
                "https://office.erpcons.vn/dang-ky/xac-minh/#token={TOKEN}"
            )),
            "{}",
            r.body
        );
        assert!(r.body.contains("48 giờ"));
        assert!(!r.subject.contains("Nguyễn"));
    }

    #[test]
    fn a_typed_name_cannot_inject_lines_or_links() {
        let r = render_signup_verify(
            "Trúng thưởng\r\nBcc: x@y.z tại https://evil.example/nhan",
            TOKEN,
            "https://office.erpcons.vn",
        );
        let greeting = r.body.lines().next().expect("greeting");
        assert!(!greeting.contains("https://evil"), "{greeting}");
        assert!(greeting.contains("hxxps://evil[.]example"), "{greeting}");
        assert!(
            r.body
                .contains("https://office.erpcons.vn/dang-ky/xac-minh/#token=")
        );
        let long = render_signup_verify(&"x".repeat(300), TOKEN, "https://office.erpcons.vn");
        assert!(long.body.lines().next().expect("greeting").chars().count() < 80);
    }

    #[test]
    fn an_empty_name_gets_a_plain_greeting() {
        let r = render_signup_verify("  ", TOKEN, "https://office.erpcons.vn");
        assert!(r.body.starts_with("Xin chào,\n"));
    }

    #[test]
    fn the_existing_account_mail_points_at_the_site() {
        let r = render_signup_existing("https://office.erpcons.vn/");
        assert!(
            r.body
                .contains("https://office.erpcons.vn/ bằng mật khẩu PI ID")
        );
        assert!(r.body.contains("Quên mật khẩu"));
    }
}
