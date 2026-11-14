//! DPoP (RFC 9449) proof signing with a P-256 key, as used by the Scalable
//! Capital API. Every HTTP request to the issuer and the GraphQL endpoint
//! carries a `DPoP` header: an ES256 JWS whose header embeds the public JWK and
//! whose claims bind method, URL, time, a random id, optionally the server
//! nonce and the SHA-256 hash of the access token (`ath`).

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use p256::ecdsa::{signature::Signer, Signature, SigningKey};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::error::{CoreError, CoreResult};

pub struct DpopKey {
    key: SigningKey,
}

impl DpopKey {
    /// Fresh random key (one per login).
    pub fn generate() -> Self {
        loop {
            let bytes: [u8; 32] = rand::random();
            if let Ok(key) = SigningKey::from_slice(&bytes) {
                return DpopKey { key };
            }
        }
    }

    /// Restores a key persisted with [`to_hex`](Self::to_hex).
    pub fn from_hex(hex_str: &str) -> CoreResult<Self> {
        let bytes = hex::decode(hex_str.trim()).map_err(|_| CoreError::Protocol {
            message: "DPoP key: invalid hex".into(),
        })?;
        let key = SigningKey::from_slice(&bytes).map_err(|_| CoreError::Protocol {
            message: "DPoP key: invalid scalar".into(),
        })?;
        Ok(DpopKey { key })
    }

    pub fn to_hex(&self) -> String {
        hex::encode(self.key.to_bytes())
    }

    /// Public key as a JWK (`{"kty":"EC","crv":"P-256","x":..,"y":..}`).
    pub fn public_jwk(&self) -> Value {
        let point = self.key.verifying_key().to_sec1_point(false);
        let x = point
            .x()
            .map(|v| URL_SAFE_NO_PAD.encode(v))
            .unwrap_or_default();
        let y = point
            .y()
            .map(|v| URL_SAFE_NO_PAD.encode(v))
            .unwrap_or_default();
        json!({"kty": "EC", "crv": "P-256", "x": x, "y": y})
    }

    /// Proof JWT for one request. `nonce` is the value of a previous
    /// `DPoP-Nonce` response header; `access_token` adds the `ath` claim
    /// (resource requests only, not the token endpoint).
    pub fn proof(
        &self,
        method: &str,
        url: &str,
        nonce: Option<&str>,
        access_token: Option<&str>,
    ) -> String {
        let iat = chrono::Utc::now().timestamp();
        let jti = uuid::Uuid::new_v4().simple().to_string();
        self.proof_at(method, url, nonce, access_token, iat, &jti)
    }

    pub(crate) fn proof_at(
        &self,
        method: &str,
        url: &str,
        nonce: Option<&str>,
        access_token: Option<&str>,
        iat: i64,
        jti: &str,
    ) -> String {
        let mut claims = json!({
            "htm": method.to_ascii_uppercase(),
            "htu": canonical_htu(url),
            "iat": iat,
            "jti": jti,
        });
        if let Some(n) = nonce.filter(|n| !n.is_empty()) {
            claims["nonce"] = Value::String(n.to_string());
        }
        if let Some(t) = access_token.filter(|t| !t.is_empty()) {
            claims["ath"] = Value::String(URL_SAFE_NO_PAD.encode(Sha256::digest(t.as_bytes())));
        }
        let header = json!({"typ": "dpop+jwt", "alg": "ES256", "jwk": self.public_jwk()});
        let input = format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(header.to_string()),
            URL_SAFE_NO_PAD.encode(claims.to_string())
        );
        let sig: Signature = self.key.sign(input.as_bytes());
        format!("{input}.{}", URL_SAFE_NO_PAD.encode(sig.to_bytes()))
    }
}

/// `htu` is the request URL without query string and fragment.
fn canonical_htu(url: &str) -> String {
    let end = url.find(['?', '#']).unwrap_or(url.len());
    url[..end].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use p256::ecdsa::{signature::Verifier, VerifyingKey};

    fn decode(part: &str) -> Value {
        serde_json::from_slice(&URL_SAFE_NO_PAD.decode(part).unwrap()).unwrap()
    }

    #[test]
    fn proof_has_required_claims_and_verifies() {
        let key = DpopKey::generate();
        let jwt = key.proof_at(
            "post",
            "https://de.scalable.capital/api/cli/graphql?x=1#frag",
            Some("nonce-1"),
            Some("token-1"),
            1_700_000_000,
            "jti-1",
        );
        let parts: Vec<&str> = jwt.split('.').collect();
        assert_eq!(parts.len(), 3);
        let header = decode(parts[0]);
        let claims = decode(parts[1]);
        assert_eq!(header["typ"], "dpop+jwt");
        assert_eq!(header["alg"], "ES256");
        assert_eq!(header["jwk"]["kty"], "EC");
        assert_eq!(header["jwk"]["crv"], "P-256");
        assert_eq!(claims["htm"], "POST");
        assert_eq!(claims["htu"], "https://de.scalable.capital/api/cli/graphql");
        assert_eq!(claims["iat"], 1_700_000_000);
        assert_eq!(claims["jti"], "jti-1");
        assert_eq!(claims["nonce"], "nonce-1");
        // ath = base64url(sha256("token-1"))
        assert_eq!(
            claims["ath"],
            URL_SAFE_NO_PAD.encode(Sha256::digest(b"token-1"))
        );
        // signature verifies with the embedded public key
        let x = URL_SAFE_NO_PAD
            .decode(header["jwk"]["x"].as_str().unwrap())
            .unwrap();
        let y = URL_SAFE_NO_PAD
            .decode(header["jwk"]["y"].as_str().unwrap())
            .unwrap();
        let mut sec1 = vec![4u8];
        sec1.extend(x);
        sec1.extend(y);
        let vk = VerifyingKey::from_sec1_bytes(&sec1).unwrap();
        let sig = Signature::from_slice(&URL_SAFE_NO_PAD.decode(parts[2]).unwrap()).unwrap();
        vk.verify(format!("{}.{}", parts[0], parts[1]).as_bytes(), &sig)
            .unwrap();
    }

    #[test]
    fn proof_without_nonce_and_token_omits_claims() {
        let key = DpopKey::generate();
        let jwt = key.proof_at("POST", "https://x.test/oauth/token", None, None, 1, "j");
        let claims = decode(jwt.split('.').nth(1).unwrap());
        assert!(claims.get("nonce").is_none());
        assert!(claims.get("ath").is_none());
    }

    #[test]
    fn key_round_trips_through_hex() {
        let key = DpopKey::generate();
        let again = DpopKey::from_hex(&key.to_hex()).unwrap();
        assert_eq!(key.public_jwk(), again.public_jwk());
        assert!(DpopKey::from_hex("zz").is_err());
    }
}
