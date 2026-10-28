use std::time::{Duration, SystemTime, UNIX_EPOCH};

use office_identity::{
    IdentityVerifier, JwtVerifier, JwtVerifierConfig, VerifyError,
    testing::{DEFAULT_NAME, DEFAULT_SUBJECT, FakePi, SigningKey, segment},
};
use serde_json::{Value, json};
use time::OffsetDateTime;

const JWKS: &str = "/id/jwks.json";
const USERINFO: &str = "/id/userinfo";
const REFUSED: Result<(), VerifyError> = Err(VerifyError::Unauthenticated);

fn verifier(pi: &FakePi) -> JwtVerifier {
    verifier_with(pi, |_| {})
}

fn verifier_with(pi: &FakePi, tweak: impl FnOnce(&mut JwtVerifierConfig)) -> JwtVerifier {
    let mut config = JwtVerifierConfig::new(pi.issuer());
    tweak(&mut config);
    JwtVerifier::new(config).expect("verifier")
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_secs() as i64
}

/// The verdict without the actor, for comparing refusals.
async fn outcome(verifier: &JwtVerifier, token: &str) -> Result<(), VerifyError> {
    verifier.verify(token).await.map(drop)
}

fn with(mut claims: Value, key: &str, value: Value) -> Value {
    claims[key] = value;
    claims
}

#[tokio::test]
async fn valid_token_resolves_to_the_actor_and_is_cached() {
    let pi = FakePi::start().await;
    let token = pi.token();
    let verifier = verifier(&pi);
    assert_eq!(verifier.cached(&token), None, "nothing cached yet");
    let actor = verifier.verify(&token).await.expect("verified");
    assert_eq!(actor.actor_id, DEFAULT_SUBJECT);
    assert_eq!(actor.display_name, DEFAULT_NAME);
    assert!(actor.expires_at > OffsetDateTime::now_utc());
    assert!(actor.expires_at <= OffsetDateTime::now_utc() + Duration::from_secs(61));
    assert_eq!(verifier.verify(&token).await, Ok(actor.clone()));
    assert_eq!(verifier.cached(&token), Some(Ok(actor)));
    assert_eq!((pi.count(JWKS), pi.count(USERINFO)), (1, 1));

    let second = pi.token();
    assert_ne!(second, token);
    assert!(verifier.verify(&second).await.is_ok());
    assert_eq!(
        (pi.count(JWKS), pi.count(USERINFO)),
        (1, 1),
        "keys and the name are reused across tokens"
    );
    assert!(
        pi.requests().iter().all(|t| !t.contains(&token)),
        "the token travels only in the Authorization header"
    );
}

#[tokio::test]
async fn concurrent_checks_of_one_token_share_one_flight() {
    let pi = FakePi::start().await;
    pi.set_delay(Duration::from_millis(200));
    let token = pi.token();
    let verifier = verifier(&pi);
    let (a, b, c) = tokio::join!(
        verifier.verify(&token),
        verifier.verify(&token),
        verifier.verify(&token)
    );
    assert_eq!(a.as_ref().map(|a| a.actor_id.as_str()), Ok(DEFAULT_SUBJECT));
    assert_eq!((a.clone(), a), (b, c));
    assert_eq!((pi.count(JWKS), pi.count(USERINFO)), (1, 1));
}

#[tokio::test]
async fn only_eddsa_tokens_of_type_jwt_reach_the_keys() {
    let pi = FakePi::start().await;
    let verifier = verifier(&pi);
    let claims = pi.claims_for(DEFAULT_SUBJECT);
    let kid = pi.kid();
    let none = format!(
        "{}.{}.",
        segment(&json!({ "alg": "none", "typ": "JWT", "kid": kid })),
        segment(&claims)
    );
    let hs256 = pi.sign_with_header(
        &json!({ "alg": "HS256", "typ": "JWT", "kid": kid }),
        &claims,
    );
    let lease = pi.sign_with_header(
        &json!({ "alg": "EdDSA", "typ": "pi-lease+jwt", "kid": kid }),
        &claims,
    );
    let no_kid = pi.sign_with_header(&json!({ "alg": "EdDSA", "typ": "JWT" }), &claims);
    for token in [&none, &hs256, &lease, &no_kid] {
        assert_eq!(
            verifier.cached(token),
            Some(Err(VerifyError::Unauthenticated))
        );
        assert_eq!(outcome(&verifier, token).await, REFUSED);
    }
    for token in ["", "two words", "not-a-jwt", "a.b.c"] {
        assert_eq!(
            verifier.cached(token),
            Some(Err(VerifyError::Unauthenticated))
        );
        assert_eq!(outcome(&verifier, token).await, REFUSED);
    }
    assert_eq!(pi.requests().len(), 0, "refused before any key lookup");
}

#[tokio::test]
async fn bad_signatures_are_refused() {
    let pi = FakePi::start().await;
    let verifier = verifier(&pi);
    let token = pi.token();
    assert!(verifier.verify(&token).await.is_ok());

    let (head, rest) = token.split_once('.').expect("dot");
    let (_, signature) = rest.split_once('.').expect("dot");
    let raised = with(pi.claims_for(DEFAULT_SUBJECT), "sub", json!("someone-else"));
    let tampered = format!("{head}.{}.{signature}", segment(&raised));
    assert_eq!(outcome(&verifier, &tampered).await, REFUSED);

    let impostor = SigningKey::generate(&pi.kid());
    let forged = impostor.sign(&impostor.header(), &pi.claims_for(DEFAULT_SUBJECT));
    assert_eq!(outcome(&verifier, &forged).await, REFUSED);
    assert_eq!(
        pi.count(USERINFO),
        1,
        "a refused token is never shown to PI"
    );
}

#[tokio::test]
async fn a_new_key_is_fetched_when_its_kid_appears() {
    let pi = FakePi::start().await;
    let verifier = verifier_with(&pi, |c| c.jwks_min_refresh = Duration::ZERO);
    assert!(verifier.verify(&pi.token()).await.is_ok());
    pi.rotate("2027-04", true);
    let rotated = pi.token();
    assert_eq!(outcome(&verifier, &rotated).await, Ok(()));
    assert_eq!(pi.count(JWKS), 2);

    pi.rotate("never-published", false);
    assert_eq!(outcome(&verifier, &pi.token()).await, REFUSED);
    assert_eq!(
        pi.count(JWKS),
        3,
        "an unknown kid refreshes once, then is refused"
    );
}

#[tokio::test]
async fn unknown_kids_refresh_the_keys_at_most_once_per_interval() {
    let pi = FakePi::start().await;
    let verifier = verifier_with(&pi, |c| c.jwks_min_refresh = Duration::from_millis(400));
    assert!(verifier.verify(&pi.token()).await.is_ok());
    let claims = pi.claims_for(DEFAULT_SUBJECT);
    for n in 0..20 {
        let header = json!({ "alg": "EdDSA", "typ": "JWT", "kid": format!("forged-{n}") });
        let forged = pi.sign_with_header(&header, &claims);
        assert_eq!(outcome(&verifier, &forged).await, REFUSED);
    }
    assert_eq!(pi.count(JWKS), 1, "forged kids cause no fetch storm");

    pi.rotate("2027-04", true);
    let rotated = pi.token();
    assert_eq!(
        outcome(&verifier, &rotated).await,
        REFUSED,
        "too soon to refetch"
    );
    tokio::time::sleep(Duration::from_millis(450)).await;
    let rotated = pi.token();
    assert_eq!(outcome(&verifier, &rotated).await, Ok(()));
    assert_eq!(pi.count(JWKS), 2);
}

#[tokio::test]
async fn claims_are_checked_after_the_signature() {
    let pi = FakePi::start().await;
    let verifier = verifier(&pi);
    let now = unix_now();
    let claims = || pi.claims_for(DEFAULT_SUBJECT);
    let refused = [
        (
            "wrong iss",
            with(claims(), "iss", json!("https://id.pi.example")),
        ),
        ("expired", with(claims(), "exp", json!(now - 120))),
        ("not yet valid", with(claims(), "nbf", json!(now + 120))),
        (
            "issued in the future",
            with(claims(), "iat", json!(now + 120)),
        ),
        (
            "aud without pi-platform",
            with(claims(), "aud", json!(["ubotbuild"])),
        ),
        (
            "a token for ubotbuild",
            with(claims(), "azp", json!("ubotbuild")),
        ),
        (
            "the desktop client by default",
            with(claims(), "azp", json!("erpcons-office-desktop")),
        ),
        ("empty sub", with(claims(), "sub", json!(""))),
        ("long sub", with(claims(), "sub", json!("x".repeat(256)))),
    ];
    for (case, claims) in refused {
        assert_eq!(
            outcome(&verifier, &pi.sign(&claims)).await,
            REFUSED,
            "{case}"
        );
    }
    let accepted = [
        (
            "aud with a product",
            with(claims(), "aud", json!(["pi-platform", "erpcons"])),
        ),
        (
            "aud as a string",
            with(claims(), "aud", json!("pi-platform")),
        ),
        (
            "expired within leeway",
            with(claims(), "exp", json!(now - 5)),
        ),
    ];
    for (case, claims) in accepted {
        assert_eq!(
            outcome(&verifier, &pi.sign(&claims)).await,
            Ok(()),
            "{case}"
        );
    }
}

#[tokio::test]
async fn allowed_clients_and_audiences_are_configurable() {
    let pi = FakePi::start().await;
    let verifier = verifier_with(&pi, |c| {
        c.allowed_clients = vec!["erpcons-office-web".into(), "erpcons-office-desktop".into()];
        c.audiences = vec!["erpcons".into()];
    });
    let claims = with(
        pi.claims_for(DEFAULT_SUBJECT),
        "azp",
        json!("erpcons-office-desktop"),
    );
    assert_eq!(
        outcome(&verifier, &pi.sign(&claims)).await,
        REFUSED,
        "no erpcons aud"
    );
    let claims = with(claims, "aud", json!(["pi-platform", "erpcons"]));
    assert_eq!(outcome(&verifier, &pi.sign(&claims)).await, Ok(()));
}

#[tokio::test]
async fn pi_down_after_the_keys_are_cached_still_authenticates() {
    let pi = FakePi::start().await;
    let verifier = verifier_with(&pi, |c| {
        c.jwks_ttl = Duration::ZERO;
        c.name_ttl = Duration::ZERO;
        c.jwks_min_refresh = Duration::ZERO;
    });
    assert!(verifier.verify(&pi.token()).await.is_ok());
    pi.set_down(true);
    pi.add_user("usr_binh", "Trần Thị Bình");

    let known = verifier.verify(&pi.token()).await.expect("known actor");
    assert_eq!(
        known.display_name, DEFAULT_NAME,
        "the last known name stands in"
    );
    let first_seen = verifier
        .verify(&pi.token_for("usr_binh"))
        .await
        .expect("new actor");
    assert_eq!(first_seen.actor_id, "usr_binh");
    assert_eq!(first_seen.display_name, "", "no name known yet");
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        pi.count(JWKS) >= 2,
        "stale keys were refreshed in the background"
    );

    pi.set_down(false);
    tokio::time::sleep(Duration::from_millis(50)).await;
    let back = verifier.verify(&pi.token()).await.expect("PI back");
    assert_eq!(back.display_name, DEFAULT_NAME);
}

#[tokio::test]
async fn a_failed_name_lookup_waits_before_asking_again() {
    let pi = FakePi::start().await;
    pi.add_user("usr_binh", "Trần Thị Bình");
    let verifier = verifier_with(&pi, |c| c.name_retry = Duration::from_millis(300));
    assert!(verifier.verify(&pi.token()).await.is_ok());
    pi.set_down(true);
    let first = verifier
        .verify(&pi.token_for("usr_binh"))
        .await
        .expect("ok");
    assert_eq!(first.display_name, "");
    pi.set_down(false);
    let soon = verifier
        .verify(&pi.token_for("usr_binh"))
        .await
        .expect("ok");
    assert_eq!(
        soon.display_name, "",
        "no new lookup within the retry delay"
    );
    assert_eq!(pi.count(USERINFO), 2);
    tokio::time::sleep(Duration::from_millis(350)).await;
    let later = verifier
        .verify(&pi.token_for("usr_binh"))
        .await
        .expect("ok");
    assert_eq!(later.display_name, "Trần Thị Bình");
}

#[tokio::test]
async fn a_name_pi_refuses_to_give_never_fails_the_request() {
    let pi = FakePi::start().await;
    let verifier = verifier(&pi);
    pi.lock_user(DEFAULT_SUBJECT);
    let actor = verifier.verify(&pi.token()).await.expect("authenticated");
    assert_eq!(actor.display_name, "", "PI's userinfo answered 401");

    let without_profile = with(pi.claims_for("usr_x"), "scope", json!("openid"));
    pi.add_user("usr_x", "X");
    let actor = verifier
        .verify(&pi.sign(&without_profile))
        .await
        .expect("ok");
    assert_eq!(actor.display_name, "", "no profile scope, no name");
}

#[tokio::test]
async fn pi_down_before_any_key_is_unavailable_and_not_cached() {
    let pi = FakePi::start().await;
    let verifier = verifier_with(&pi, |c| c.jwks_min_refresh = Duration::ZERO);
    let token = pi.token();
    pi.set_down(true);
    assert_eq!(
        outcome(&verifier, &token).await,
        Err(VerifyError::Unavailable)
    );
    assert_eq!(verifier.cached(&token), None);
    pi.set_down(false);
    assert_eq!(outcome(&verifier, &token).await, Ok(()));
}

#[tokio::test]
async fn an_unknown_kid_while_pi_is_down_is_unavailable() {
    let pi = FakePi::start().await;
    let verifier = verifier_with(&pi, |c| c.jwks_min_refresh = Duration::ZERO);
    assert!(verifier.verify(&pi.token()).await.is_ok());
    pi.rotate("2027-04", true);
    pi.set_down(true);
    assert_eq!(
        outcome(&verifier, &pi.token()).await,
        Err(VerifyError::Unavailable),
        "a rotated key cannot be told from a forged kid until PI answers"
    );
}

#[tokio::test]
async fn slow_or_redirecting_pi_is_unavailable() {
    let pi = FakePi::start().await;
    pi.set_delay(Duration::from_secs(2));
    let impatient = verifier_with(&pi, |c| {
        c.timeout = Duration::from_millis(200);
        c.jwks_min_refresh = Duration::ZERO;
    });
    assert_eq!(
        outcome(&impatient, &pi.token()).await,
        Err(VerifyError::Unavailable)
    );

    let pi = FakePi::start().await;
    let elsewhere = FakePi::start().await;
    pi.set_redirect(Some(&format!("{}/id/jwks.json", elsewhere.issuer())));
    let redirected = verifier(&pi);
    assert_eq!(
        outcome(&redirected, &pi.token()).await,
        Err(VerifyError::Unavailable)
    );
    assert_eq!(elsewhere.requests().len(), 0, "redirects are not followed");
}

#[tokio::test]
async fn unreachable_pi_is_unavailable() {
    let verifier =
        JwtVerifier::new(JwtVerifierConfig::new("http://127.0.0.1:1")).expect("verifier");
    let pi = FakePi::start().await;
    assert_eq!(
        outcome(&verifier, &pi.token()).await,
        Err(VerifyError::Unavailable)
    );
}
