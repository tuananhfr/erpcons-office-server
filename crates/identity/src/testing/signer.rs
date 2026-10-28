use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use ring::{
    rand::SystemRandom,
    signature::{ED25519, Ed25519KeyPair, KeyPair, UnparsedPublicKey},
};
use serde_json::{Value, json};

/// A fresh Ed25519 key under a `kid`, as in PI's `pi_id_signing_keys`.
pub struct SigningKey {
    kid: String,
    pair: Ed25519KeyPair,
}

impl SigningKey {
    pub fn generate(kid: &str) -> Self {
        let pkcs8 = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new()).expect("Ed25519 key");
        Self {
            kid: kid.to_owned(),
            pair: Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).expect("Ed25519 key"),
        }
    }

    pub fn kid(&self) -> &str {
        &self.kid
    }

    /// The header PI puts on an access token (Jwt.php `sign`).
    pub fn header(&self) -> Value {
        json!({ "alg": "EdDSA", "typ": "JWT", "kid": self.kid })
    }

    /// A compact JWS of `claims` under `header`, signed with this key whatever the header says.
    pub fn sign(&self, header: &Value, claims: &Value) -> String {
        let input = format!("{}.{}", segment(header), segment(claims));
        let signature = self.pair.sign(input.as_bytes());
        format!("{input}.{}", URL_SAFE_NO_PAD.encode(signature.as_ref()))
    }

    /// The JWK PI publishes for this key (Jwt.php `jwks`).
    pub fn jwk(&self) -> Value {
        json!({
            "kty": "OKP",
            "crv": "Ed25519",
            "x": URL_SAFE_NO_PAD.encode(self.pair.public_key().as_ref()),
            "kid": self.kid,
            "use": "sig",
            "alg": "EdDSA",
        })
    }

    pub(crate) fn verifies(&self, input: &[u8], signature: &[u8]) -> bool {
        UnparsedPublicKey::new(&ED25519, self.pair.public_key().as_ref())
            .verify(input, signature)
            .is_ok()
    }
}

/// One unpadded base64url JSON segment, for hand-made tokens (`alg: none`).
pub fn segment(value: &Value) -> String {
    URL_SAFE_NO_PAD.encode(value.to_string())
}
