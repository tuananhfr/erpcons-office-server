//! Test helpers: sign a user in on an in-process `pi_mock::Server` and get an access token.
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use reqwest::{Url, header::LOCATION, redirect};
use serde::Deserialize;
use sha2::{Digest, Sha256};

const CLIENT_ID: &str = "erpcons-office-web";

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
}

/// Access token for the scenario's `sign_in_as` user, through the web client's code + PKCE flow.
pub async fn access_token(server: &pi_mock::Server) -> String {
    let redirect_uri = server
        .scenario()
        .web_redirect_uris
        .first()
        .cloned()
        .expect("pi-mock has a web redirect URI");
    let verifier = "office-server-test-verifier-".repeat(3);
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let http = reqwest::Client::builder()
        .redirect(redirect::Policy::none())
        .build()
        .expect("http client");
    let authorize = http
        .get(format!("{}/authorize", server.base_url()))
        .query(&[
            ("client_id", CLIENT_ID),
            ("redirect_uri", redirect_uri.as_str()),
            ("response_type", "code"),
            ("scope", "openid profile"),
            ("state", "test-state"),
            ("nonce", "test-nonce"),
            ("code_challenge", challenge.as_str()),
            ("code_challenge_method", "S256"),
        ])
        .send()
        .await
        .expect("authorize");
    let location = authorize
        .headers()
        .get(LOCATION)
        .and_then(|v| v.to_str().ok())
        .expect("authorize redirects back");
    let code = Url::parse(location)
        .expect("redirect URL")
        .query_pairs()
        .find(|(k, _)| k == "code")
        .map(|(_, v)| v.into_owned())
        .expect("authorization code");
    let token: TokenResponse = http
        .post(format!("{}/token", server.base_url()))
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", code.as_str()),
            ("redirect_uri", redirect_uri.as_str()),
            ("code_verifier", verifier.as_str()),
            ("client_id", CLIENT_ID),
        ])
        .send()
        .await
        .expect("token request")
        .error_for_status()
        .expect("token issued")
        .json()
        .await
        .expect("token body");
    token.access_token
}

/// Access token for `user_id`. Switching the signed-in user resets pi-mock's fault counters.
pub async fn access_token_as(server: &pi_mock::Server, user_id: &str) -> String {
    server.update_scenario(|s| s.sign_in_as = user_id.to_owned());
    access_token(server).await
}
