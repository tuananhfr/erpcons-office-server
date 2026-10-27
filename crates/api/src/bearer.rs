use axum::http::{HeaderMap, header::AUTHORIZATION};

/// The token of a single well-formed `Authorization: Bearer <token>` header (RFC 6750 §2.1).
pub(crate) fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    let mut values = headers.get_all(AUTHORIZATION).iter();
    let value = values.next()?;
    if values.next().is_some() {
        return None;
    }
    let (scheme, token) = value.to_str().ok()?.split_once(' ')?;
    (scheme.eq_ignore_ascii_case("bearer") && is_token68(token)).then_some(token)
}

fn is_token68(s: &str) -> bool {
    let body = s.trim_end_matches('=');
    !body.is_empty()
        && body
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-._~+/".contains(&b))
}

#[cfg(test)]
mod tests {
    use axum::http::HeaderValue;

    use super::*;

    fn headers(values: &[&str]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for v in values {
            map.append(AUTHORIZATION, HeaderValue::from_str(v).expect("header"));
        }
        map
    }

    #[test]
    fn accepts_bearer_tokens() {
        assert_eq!(
            bearer_token(&headers(&["Bearer abc.DEF-1_~+/="])),
            Some("abc.DEF-1_~+/=")
        );
        assert_eq!(bearer_token(&headers(&["bearer abc"])), Some("abc"));
    }

    #[test]
    fn rejects_everything_else() {
        for bad in [
            "",
            "Bearer",
            "Bearer ",
            "Basic abc",
            "Bearer a b",
            "Bearer  abc",
            "Bearer ===",
            "Bearer a=b",
        ] {
            assert_eq!(bearer_token(&headers(&[bad])), None, "{bad:?}");
        }
        assert_eq!(bearer_token(&HeaderMap::new()), None);
        assert_eq!(bearer_token(&headers(&["Bearer a", "Bearer b"])), None);
    }
}
