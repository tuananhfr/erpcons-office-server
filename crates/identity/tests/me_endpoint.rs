use std::time::{Duration, SystemTime, UNIX_EPOCH};

use office_identity::{
    IdentityVerifier, MeEndpointConfig, MeEndpointVerifier, VerifyError, testing::access_token,
};
use pi_mock::{DEFAULT_USER, Fault, Scenario, Server};
use time::OffsetDateTime;

fn verifier(server: &Server) -> MeEndpointVerifier {
    verifier_with(server, |_| {})
}

fn verifier_with(server: &Server, tweak: impl FnOnce(&mut MeEndpointConfig)) -> MeEndpointVerifier {
    let mut config = MeEndpointConfig::new(server.base_url());
    tweak(&mut config);
    MeEndpointVerifier::new(config).expect("verifier")
}

fn me_calls(server: &Server) -> usize {
    server
        .requests()
        .iter()
        .filter(|r| r.path == "/v1/me")
        .count()
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_secs() as i64
}

#[tokio::test]
async fn valid_token_resolves_to_the_actor_and_is_cached() {
    let server = Server::start(Scenario::default());
    let token = access_token(&server).await;
    let verifier = verifier(&server);
    let actor = verifier.verify(&token).await.expect("verified");
    assert_eq!(actor.actor_id, DEFAULT_USER);
    assert_eq!(actor.display_name, "Nguyễn Văn An");
    assert!(actor.expires_at > OffsetDateTime::now_utc());
    assert_eq!(verifier.verify(&token).await, Ok(actor));
    assert_eq!(
        me_calls(&server),
        1,
        "the second call is served from the cache"
    );
    let leaked = server
        .requests()
        .iter()
        .any(|r| r.path == "/v1/me" && (r.query.contains(&token) || r.body.contains(&token)));
    assert!(
        !leaked,
        "the token travels only in the Authorization header"
    );
}

#[tokio::test]
async fn unknown_token_is_rejected_and_cached_only_briefly() {
    let server = Server::start(Scenario::default());
    let verifier = verifier_with(&server, |c| c.negative_ttl = Duration::from_millis(200));
    assert_eq!(
        verifier.verify("not-a-token").await,
        Err(VerifyError::Unauthenticated)
    );
    assert_eq!(
        verifier.verify("not-a-token").await,
        Err(VerifyError::Unauthenticated)
    );
    assert_eq!(me_calls(&server), 1);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        verifier.verify("not-a-token").await,
        Err(VerifyError::Unauthenticated)
    );
    assert_eq!(me_calls(&server), 2);
}

#[tokio::test]
async fn malformed_token_is_rejected_without_asking_pi() {
    let server = Server::start(Scenario::default());
    let verifier = verifier(&server);
    for token in ["", "two words", "line\r\nbreak"] {
        assert_eq!(
            verifier.verify(token).await,
            Err(VerifyError::Unauthenticated)
        );
    }
    assert_eq!(me_calls(&server), 0);
}

#[tokio::test]
async fn expired_token_is_rejected() {
    let server = Server::start(Scenario::default());
    let token = access_token(&server).await;
    server.update_scenario(|s| s.now = Some(unix_now() + 7200));
    let verdict = verifier(&server).verify(&token).await;
    assert_eq!(verdict, Err(VerifyError::Unauthenticated));
}

#[tokio::test]
async fn suspended_user_is_rejected() {
    let server = Server::start(Scenario::default());
    let token = access_token(&server).await;
    server.update_scenario(|s| s.users[0].suspended = true);
    let verdict = verifier(&server).verify(&token).await;
    assert_eq!(verdict, Err(VerifyError::Unauthenticated));
}

#[tokio::test]
async fn pi_errors_are_unavailable_and_not_cached() {
    let server = Server::start(Scenario::default());
    let token = access_token(&server).await;
    let verifier = verifier(&server);
    for status in [500, 503, 429] {
        server.add_fault(Fault::problem("GET", "/v1/me", status, "unavailable"));
        assert_eq!(
            verifier.verify(&token).await,
            Err(VerifyError::Unavailable),
            "{status}"
        );
        server.clear_faults();
    }
    assert!(
        verifier.verify(&token).await.is_ok(),
        "errors are not cached"
    );
}

#[tokio::test]
async fn pi_timeout_is_unavailable() {
    let server = Server::start(Scenario::default());
    let token = access_token(&server).await;
    server.add_fault(Fault::delay("GET", "/v1/me", Duration::from_secs(2)));
    let verifier = verifier_with(&server, |c| c.timeout = Duration::from_millis(200));
    assert_eq!(verifier.verify(&token).await, Err(VerifyError::Unavailable));
}

#[tokio::test]
async fn unreachable_pi_is_unavailable() {
    let config = MeEndpointConfig::new("http://127.0.0.1:1");
    let verifier = MeEndpointVerifier::new(config).expect("verifier");
    assert_eq!(
        verifier.verify("some-token").await,
        Err(VerifyError::Unavailable)
    );
}
