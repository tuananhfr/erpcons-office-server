use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use ring::signature::{ED25519, UnparsedPublicKey};
use serde::Deserialize;

use crate::jwks::PublicKey;

pub(crate) const MAX_TOKEN_LEN: usize = 4096;
const MAX_ACTOR_ID_LEN: usize = 255;
const SIGNATURE_LEN: usize = 64;

/// Why a token was refused. Logged by name only, never with any part of the token.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Reject {
    Malformed,
    Algorithm,
    Type,
    KeyId,
    Critical,
    Signature,
    Issuer,
    Expired,
    NotYetValid,
    Audience,
    Client,
    Subject,
}

impl Reject {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Malformed => "malformed",
            Self::Algorithm => "alg",
            Self::Type => "typ",
            Self::KeyId => "kid",
            Self::Critical => "crit",
            Self::Signature => "signature",
            Self::Issuer => "iss",
            Self::Expired => "exp",
            Self::NotYetValid => "nbf_iat",
            Self::Audience => "aud",
            Self::Client => "azp",
            Self::Subject => "sub",
        }
    }
}

#[derive(Deserialize)]
struct Header {
    alg: Option<String>,
    typ: Option<String>,
    kid: Option<String>,
    crit: Option<serde_json::Value>,
}

/// A token whose header passed and whose signature is still to be checked.
pub(crate) struct Unverified<'a> {
    pub(crate) kid: String,
    signing_input: &'a str,
    signature: Vec<u8>,
    payload: Vec<u8>,
}

/// Splits a compact JWS and checks its header before any key is looked up, so `none`, `HS256` or
/// another type of PI token (`pi-lease+jwt`) never reach the signature check.
pub(crate) fn split(token: &str) -> Result<Unverified<'_>, Reject> {
    if token.len() > MAX_TOKEN_LEN {
        return Err(Reject::Malformed);
    }
    let mut parts = token.split('.');
    let (Some(header), Some(payload), Some(signature), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(Reject::Malformed);
    };
    let head: Header = serde_json::from_slice(&segment(header)?).map_err(|_| Reject::Malformed)?;
    if head.alg.as_deref() != Some("EdDSA") {
        return Err(Reject::Algorithm);
    }
    // PI signs access tokens with `typ: JWT` and other tokens with their own type (Jwt.php).
    if head.typ.as_deref() != Some("JWT") {
        return Err(Reject::Type);
    }
    if head.crit.is_some() {
        return Err(Reject::Critical);
    }
    let kid = head.kid.filter(|k| valid_kid(k)).ok_or(Reject::KeyId)?;
    let signature = segment(signature)?;
    if signature.len() != SIGNATURE_LEN {
        return Err(Reject::Signature);
    }
    Ok(Unverified {
        kid,
        signing_input: &token[..header.len() + 1 + payload.len()],
        signature,
        payload: segment(payload)?,
    })
}

impl Unverified<'_> {
    /// The payload, once the signature checks out against `key`.
    pub(crate) fn verify(self, key: &PublicKey) -> Result<Vec<u8>, Reject> {
        UnparsedPublicKey::new(&ED25519, key)
            .verify(self.signing_input.as_bytes(), &self.signature)
            .map_err(|_| Reject::Signature)?;
        Ok(self.payload)
    }
}

/// Unpadded base64url only (RFC 7515 §2); padding or stray bits are refused.
fn segment(part: &str) -> Result<Vec<u8>, Reject> {
    URL_SAFE_NO_PAD.decode(part).map_err(|_| Reject::Malformed)
}

/// PI's own rule for key ids (Jwt.php `keys()`).
pub(crate) fn valid_kid(kid: &str) -> bool {
    (1..=64).contains(&kid.len())
        && kid
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}

/// What a token must say to be accepted.
pub(crate) struct Policy<'a> {
    pub(crate) issuer: &'a str,
    pub(crate) audiences: &'a [String],
    pub(crate) clients: &'a [String],
    pub(crate) leeway_secs: i64,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Audience {
    One(String),
    Many(Vec<String>),
}

impl Audience {
    fn contains_any(&self, accepted: &[String]) -> bool {
        match self {
            Self::One(aud) => accepted.contains(aud),
            Self::Many(auds) => auds.iter().any(|a| accepted.contains(a)),
        }
    }
}

// Unknown claims (`scope`, `org_context`, `jti`) are ignored; a claim of the wrong type fails the
// whole token.
#[derive(Deserialize)]
struct Claims {
    iss: Option<String>,
    sub: Option<String>,
    aud: Option<Audience>,
    azp: Option<String>,
    exp: Option<i64>,
    nbf: Option<i64>,
    iat: Option<i64>,
}

/// The claims Office keeps from an accepted token.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Accepted {
    pub(crate) sub: String,
    pub(crate) exp: i64,
}

/// Checks a verified payload against `policy` at `now` (Unix seconds).
pub(crate) fn check_claims(payload: &[u8], policy: &Policy, now: i64) -> Result<Accepted, Reject> {
    let claims: Claims = serde_json::from_slice(payload).map_err(|_| Reject::Malformed)?;
    if claims.iss.as_deref() != Some(policy.issuer) {
        return Err(Reject::Issuer);
    }
    let exp = claims.exp.ok_or(Reject::Expired)?;
    if now > exp.saturating_add(policy.leeway_secs) {
        return Err(Reject::Expired);
    }
    let from_future =
        |t: Option<i64>| t.is_some_and(|t| t.saturating_sub(policy.leeway_secs) > now);
    if from_future(claims.nbf) || from_future(claims.iat) {
        return Err(Reject::NotYetValid);
    }
    if !claims
        .aud
        .is_some_and(|aud| aud.contains_any(policy.audiences))
    {
        return Err(Reject::Audience);
    }
    // Every PI token carries `aud: pi-platform`; `azp` is what tells Office's clients from others.
    if !claims.azp.is_some_and(|azp| policy.clients.contains(&azp)) {
        return Err(Reject::Client);
    }
    let sub = claims
        .sub
        .filter(|s| !s.is_empty() && s.len() <= MAX_ACTOR_ID_LEN)
        .ok_or(Reject::Subject)?;
    Ok(Accepted { sub, exp })
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;

    const NOW: i64 = 1_800_000_000;

    fn b64(value: &Value) -> String {
        URL_SAFE_NO_PAD.encode(value.to_string())
    }

    fn token(header: &Value) -> String {
        format!(
            "{}.{}.{}",
            b64(header),
            b64(&json!({})),
            URL_SAFE_NO_PAD.encode([7u8; 64])
        )
    }

    fn header() -> Value {
        json!({ "alg": "EdDSA", "typ": "JWT", "kid": "2026-10" })
    }

    #[test]
    fn a_pi_header_splits() {
        let token = token(&header());
        let parts = split(&token).map_err(Reject::as_str).expect("split");
        assert_eq!(parts.kid, "2026-10");
        assert_eq!(
            parts.signing_input,
            &token[..token.rfind('.').expect("dot")]
        );
    }

    #[test]
    fn only_eddsa_is_accepted() {
        for alg in [
            json!("none"),
            json!("HS256"),
            json!("RS256"),
            json!("eddsa"),
            json!(null),
        ] {
            let mut h = header();
            h["alg"] = alg.clone();
            assert_eq!(split(&token(&h)).err(), Some(Reject::Algorithm), "{alg}");
        }
        let mut h = header();
        h.as_object_mut().expect("object").remove("alg");
        assert_eq!(split(&token(&h)).err(), Some(Reject::Algorithm));
    }

    #[test]
    fn other_pi_token_types_and_extensions_are_refused() {
        for typ in [json!("pi-lease+jwt"), json!("at+jwt"), json!(null)] {
            let mut h = header();
            h["typ"] = typ.clone();
            assert_eq!(split(&token(&h)).err(), Some(Reject::Type), "{typ}");
        }
        let mut h = header();
        h["crit"] = json!(["exp"]);
        assert_eq!(split(&token(&h)).err(), Some(Reject::Critical));
        for kid in [json!(""), json!("a/b"), json!(1), json!("k".repeat(65))] {
            let mut h = header();
            h["kid"] = kid.clone();
            let refused = split(&token(&h)).err();
            assert!(
                matches!(refused, Some(Reject::KeyId | Reject::Malformed)),
                "{kid}"
            );
        }
    }

    #[test]
    fn malformed_compact_forms_are_refused() {
        let good = token(&header());
        let (h, rest) = good.split_once('.').expect("dot");
        let padded = format!("{h}=.{rest}");
        let short_signature = format!("{}.{}.{}", b64(&header()), b64(&json!({})), "AAAA");
        for bad in [
            "",
            "abc",
            "a.b",
            "a.b.c.d",
            &format!("{good}.x"),
            &padded,
            &format!("{}.e30.!!!!", b64(&header())),
            &format!("{}.{rest}", URL_SAFE_NO_PAD.encode("not json")),
        ] {
            assert!(split(bad).is_err(), "{bad}");
        }
        assert_eq!(split(&short_signature).err(), Some(Reject::Signature));
        assert_eq!(
            split(&"a".repeat(MAX_TOKEN_LEN + 1)).err(),
            Some(Reject::Malformed)
        );
    }

    fn claims() -> Value {
        json!({
            "iss": "https://id.pi.example",
            "sub": "4f1c2a8e-6b7d-4e0a-9c3f-2d5b8a1e7f60",
            "aud": ["pi-platform"],
            "azp": "erpcons-office-web",
            "scope": "openid profile email offline_access",
            "org_context": null,
            "iat": NOW - 10,
            "nbf": NOW - 10,
            "exp": NOW + 590,
            "jti": "9b1d…",
        })
    }

    fn check(claims: &Value) -> Result<Accepted, Reject> {
        let audiences = ["pi-platform".to_owned()];
        let clients = ["erpcons-office-web".to_owned()];
        let policy = Policy {
            issuer: "https://id.pi.example",
            audiences: &audiences,
            clients: &clients,
            leeway_secs: 30,
        };
        check_claims(claims.to_string().as_bytes(), &policy, NOW)
    }

    fn with(key: &str, value: Value) -> Value {
        let mut c = claims();
        c[key] = value;
        c
    }

    fn without(key: &str) -> Value {
        let mut c = claims();
        c.as_object_mut().expect("object").remove(key);
        c
    }

    #[test]
    fn pi_access_token_claims_are_accepted() {
        assert_eq!(
            check(&claims()),
            Ok(Accepted {
                sub: "4f1c2a8e-6b7d-4e0a-9c3f-2d5b8a1e7f60".into(),
                exp: NOW + 590
            })
        );
        assert!(check(&with("aud", json!("pi-platform"))).is_ok());
        assert!(check(&with("aud", json!(["pi-platform", "erpcons"]))).is_ok());
        assert!(
            check(&with("exp", json!(NOW - 20))).is_ok(),
            "within leeway"
        );
        assert!(
            check(&with("nbf", json!(NOW + 20))).is_ok(),
            "within leeway"
        );
        assert!(check(&without("nbf")).is_ok());
        assert!(check(&without("iat")).is_ok());
    }

    #[test]
    fn issuer_must_match_exactly() {
        for iss in [
            json!("https://id.pi.example/"),
            json!("https://evil.example"),
            json!(null),
        ] {
            assert_eq!(
                check(&with("iss", iss.clone())),
                Err(Reject::Issuer),
                "{iss}"
            );
        }
        assert_eq!(check(&without("iss")), Err(Reject::Issuer));
    }

    #[test]
    fn time_claims_are_enforced() {
        assert_eq!(check(&with("exp", json!(NOW - 31))), Err(Reject::Expired));
        assert_eq!(check(&without("exp")), Err(Reject::Expired));
        assert_eq!(check(&with("exp", json!("soon"))), Err(Reject::Malformed));
        assert_eq!(check(&with("exp", json!(1.5))), Err(Reject::Malformed));
        assert_eq!(
            check(&with("nbf", json!(NOW + 31))),
            Err(Reject::NotYetValid)
        );
        assert_eq!(
            check(&with("iat", json!(NOW + 31))),
            Err(Reject::NotYetValid)
        );
        assert_eq!(check(&with("exp", json!(i64::MIN))), Err(Reject::Expired));
    }

    #[test]
    fn audience_and_client_are_enforced() {
        for aud in [
            json!(["erpcons"]),
            json!("ubotbuild"),
            json!([]),
            json!(null),
        ] {
            assert_eq!(
                check(&with("aud", aud.clone())),
                Err(Reject::Audience),
                "{aud}"
            );
        }
        assert_eq!(check(&with("aud", json!(7))), Err(Reject::Malformed));
        for azp in [
            json!("ubotbuild"),
            json!("erpcons-office-desktop"),
            json!(""),
            json!(null),
        ] {
            assert_eq!(
                check(&with("azp", azp.clone())),
                Err(Reject::Client),
                "{azp}"
            );
        }
        assert_eq!(check(&without("azp")), Err(Reject::Client));
    }

    #[test]
    fn subject_must_fit_an_actor_id() {
        assert_eq!(check(&with("sub", json!(""))), Err(Reject::Subject));
        assert_eq!(
            check(&with("sub", json!("x".repeat(256)))),
            Err(Reject::Subject)
        );
        assert!(check(&with("sub", json!("x".repeat(255)))).is_ok());
        assert_eq!(check(&without("sub")), Err(Reject::Subject));
    }

    #[test]
    fn duplicate_or_mistyped_claims_fail_the_token() {
        let duplicated = br#"{"iss":"https://id.pi.example","iss":"https://evil.example"}"#;
        let audiences = ["pi-platform".to_owned()];
        let policy = Policy {
            issuer: "https://id.pi.example",
            audiences: &audiences,
            clients: &audiences,
            leeway_secs: 0,
        };
        assert_eq!(
            check_claims(duplicated, &policy, NOW),
            Err(Reject::Malformed)
        );
        assert_eq!(check(&with("sub", json!(42))), Err(Reject::Malformed));
        assert_eq!(check_claims(b"[]", &policy, NOW), Err(Reject::Malformed));
    }
}
