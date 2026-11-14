//! bunq API protocol: request signing, server-signature check, installation,
//! device registration and session.

use std::time::{Duration, Instant};

use base64::Engine as _;
use rsa::pkcs1v15::{Signature, SigningKey, VerifyingKey};
use rsa::pkcs8::{
    DecodePrivateKey, DecodePublicKey, EncodePrivateKey, EncodePublicKey, LineEnding,
};
use rsa::signature::{SignatureEncoding, Signer, Verifier};
use rsa::{PrivateKey, PublicKey};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::parse::find;
use super::{BunqConnector, BunqState, MIN_GAP};
use crate::error::{CoreError, CoreResult};
use crate::i18n::tr;

/// Error returned by bunq: HTTP status + `Error[0].error_description`.
#[derive(Debug)]
pub(super) struct ApiError {
    pub(super) status: u16,
    pub(super) message: String,
}

impl ApiError {
    pub(super) fn into_core(self) -> CoreError {
        match self.status {
            401 | 403 => CoreError::Authentication {
                message: format!(
                    "{} ({})",
                    tr("bunq hat den API-Schlüssel oder diese IP-Adresse abgelehnt. Erstellen Sie in der bunq-App einen API-Schlüssel mit „Alle IP-Adressen erlauben“ und tragen Sie ihn neu ein."),
                    self.message
                ),
            },
            _ => CoreError::Bank {
                code: self.status.to_string(),
                message: self.message,
            },
        }
    }
}

pub(super) type ApiResult<T> = Result<T, ApiError>;

impl From<CoreError> for ApiError {
    fn from(e: CoreError) -> Self {
        ApiError {
            status: 0,
            message: e.to_string(),
        }
    }
}

impl BunqConnector {
    pub(super) fn signing_key(&self) -> CoreResult<SigningKey<Sha256>> {
        let key = PrivateKey::from_pkcs8_pem(&self.state.private_key_pem).map_err(|e| {
            CoreError::Protocol {
                message: format!("bunq key: {e}"),
            }
        })?;
        Ok(SigningKey::<Sha256>::new(key))
    }

    /// One API call. `auth` = token for `X-Bunq-Client-Authentication`.
    /// One API call returning the raw body and its content type.
    /// `body` = (bytes, content type); it is signed as sent.
    pub(super) async fn send(
        &mut self,
        method: reqwest::Method,
        path: &str,
        body: Option<(Vec<u8>, &str)>,
        extra_headers: &[(&str, &str)],
        auth: Option<&str>,
    ) -> ApiResult<(Vec<u8>, String)> {
        // Paths can come from bunq (pagination); the session token must
        // never leave the bunq host, so only plain API paths are sent.
        if !is_api_path(path) {
            return Err(ApiError {
                status: 0,
                message: format!("invalid bunq API path: {path}"),
            });
        }
        let mut attempt = 0;
        loop {
            // Throttle: bunq rate limits are per endpoint and very low for GET.
            if let Some(last) = self.last_request {
                let elapsed = last.elapsed();
                if elapsed < MIN_GAP {
                    tokio::time::sleep(MIN_GAP - elapsed).await;
                }
            }
            self.last_request = Some(Instant::now());

            let client = self.client.as_ref().ok_or(CoreError::SessionClosed)?;
            let url = format!("{}{}", self.base, path);
            let mut req = client
                .request(method.clone(), &url)
                .header("Cache-Control", "no-cache")
                .header("X-Bunq-Client-Request-Id", uuid::Uuid::new_v4().to_string())
                .header("X-Bunq-Geolocation", "0 0 0 0 000")
                .header("X-Bunq-Language", "en_US")
                .header("X-Bunq-Region", "en_US");
            for (k, v) in extra_headers {
                req = req.header(*k, *v);
            }
            if let Some(token) = auth {
                req = req.header("X-Bunq-Client-Authentication", token);
            }
            if let Some((bytes, content_type)) = &body {
                // Sign exactly the bytes we send.
                if !self.state.private_key_pem.is_empty() {
                    let sig = self.signing_key()?.sign(bytes);
                    req = req.header(
                        "X-Bunq-Client-Signature",
                        base64::engine::general_purpose::STANDARD.encode(sig.to_bytes()),
                    );
                }
                req = req
                    .header("Content-Type", *content_type)
                    .body(bytes.clone());
            }
            let resp = req.send().await.map_err(CoreError::from)?;
            let status = resp.status().as_u16();
            let header = |k: &str| {
                resp.headers()
                    .get(k)
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_string)
            };
            let server_sig = header("X-Bunq-Server-Signature");
            let content_type = header("Content-Type").unwrap_or_default();
            let raw = crate::connectors::read_body_limited(resp, crate::connectors::MAX_DOC_BYTES)
                .await?;

            if status == 429 && attempt < 4 {
                attempt += 1;
                tokio::time::sleep(Duration::from_secs(3 * attempt)).await;
                continue;
            }
            if !(200..300).contains(&status) {
                let v: Value = serde_json::from_slice(&raw).unwrap_or(Value::Null);
                return Err(ApiError {
                    status,
                    message: v["Error"][0]["error_description"]
                        .as_str()
                        .unwrap_or("bunq error")
                        .to_string(),
                });
            }
            self.verify_response(&raw, server_sig.as_deref())?;
            return Ok((raw, content_type));
        }
    }

    /// JSON API call. `auth` = token for `X-Bunq-Client-Authentication`.
    pub(super) async fn call(
        &mut self,
        method: reqwest::Method,
        path: &str,
        body: Option<&Value>,
        auth: Option<&str>,
    ) -> ApiResult<Value> {
        let body = match body {
            Some(b) => Some((
                serde_json::to_vec(b).map_err(|e| CoreError::Protocol {
                    message: e.to_string(),
                })?,
                "application/json",
            )),
            None => None,
        };
        let (raw, _) = self.send(method, path, body, &[], auth).await?;
        Ok(serde_json::from_slice(&raw).unwrap_or(Value::Null))
    }

    /// Raw GET with the session token (attachments, statement files).
    pub(super) async fn get_raw(&mut self, path: &str) -> ApiResult<(Vec<u8>, String)> {
        let token = self.state.session_token.clone();
        self.send(reqwest::Method::GET, path, None, &[], Some(&token))
            .await
    }

    /// Checks `X-Bunq-Server-Signature` (body only) once the server key is known.
    pub(super) fn verify_response(&self, raw: &[u8], sig: Option<&str>) -> CoreResult<()> {
        if self.state.server_public_key.is_empty() {
            return Ok(()); // installation response itself
        }
        let invalid = || CoreError::Protocol {
            message: tr("Die Antwort von bunq ist nicht korrekt signiert.").into(),
        };
        let sig = sig.ok_or_else(invalid)?;
        let key =
            PublicKey::from_public_key_pem(&self.state.server_public_key).map_err(|_| invalid())?;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(sig)
            .map_err(|_| invalid())?;
        let sig = Signature::try_from(bytes.as_slice()).map_err(|_| invalid())?;
        VerifyingKey::<Sha256>::new(key)
            .verify(raw, &sig)
            .map_err(|_| invalid())
    }

    pub(super) async fn get(&mut self, path: &str) -> ApiResult<Value> {
        let token = self.state.session_token.clone();
        self.call(reqwest::Method::GET, path, None, Some(&token))
            .await
    }

    /// POST /installation with a fresh 2048-bit key.
    pub(super) async fn install(&mut self) -> CoreResult<()> {
        let key = tokio::task::spawn_blocking(|| PrivateKey::new(&mut getrandom::SysRng, 2048))
            .await
            .map_err(|e| CoreError::Protocol {
                message: e.to_string(),
            })?
            .map_err(|e| CoreError::Protocol {
                message: format!("bunq key: {e}"),
            })?;
        let pem_err = |e: String| CoreError::Protocol { message: e };
        let public_pem = key
            .to_public_key()
            .to_public_key_pem(LineEnding::LF)
            .map_err(|e| pem_err(e.to_string()))?;
        self.state = BunqState {
            env: self.state.env.clone(),
            key_fp: self.state.key_fp.clone(),
            private_key_pem: key
                .to_pkcs8_pem(LineEnding::LF)
                .map_err(|e| pem_err(e.to_string()))?
                .to_string(),
            ..Default::default()
        };
        let v = self
            .call(
                reqwest::Method::POST,
                "/v1/installation",
                Some(&json!({ "client_public_key": public_pem })),
                None,
            )
            .await
            .map_err(ApiError::into_core)?;
        self.state.installation_token = find(&v, "Token")["token"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        self.state.server_public_key = find(&v, "ServerPublicKey")["server_public_key"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        if self.state.installation_token.is_empty() || self.state.server_public_key.is_empty() {
            return Err(CoreError::Protocol {
                message: "bunq installation: missing token".into(),
            });
        }
        Ok(())
    }

    /// POST /device-server. Asks for "any IP" first so the Mac keeps working
    /// when its address changes; keys bound to one IP fall back to that IP.
    pub(super) async fn register_device(&mut self, api_key: &str) -> CoreResult<()> {
        let token = self.state.installation_token.clone();
        let mut body = json!({
            "description": "GiroCrab",
            "secret": api_key,
            "permitted_ips": ["*"],
        });
        let first = self
            .call(
                reqwest::Method::POST,
                "/v1/device-server",
                Some(&body),
                Some(&token),
            )
            .await;
        if let Err(e) = first {
            if e.status != 400 {
                return Err(e.into_core());
            }
            body.as_object_mut().unwrap().remove("permitted_ips");
            self.call(
                reqwest::Method::POST,
                "/v1/device-server",
                Some(&body),
                Some(&token),
            )
            .await
            .map_err(ApiError::into_core)?;
        }
        self.state.device_registered = true;
        Ok(())
    }

    /// POST /session-server -> session token + user id.
    pub(super) async fn open_session(&mut self, api_key: &str) -> ApiResult<()> {
        let token = self.state.installation_token.clone();
        let v = self
            .call(
                reqwest::Method::POST,
                "/v1/session-server",
                Some(&json!({ "secret": api_key })),
                Some(&token),
            )
            .await?;
        let session = find(&v, "Token")["token"].as_str().unwrap_or_default();
        let user_id = [
            "UserPerson",
            "UserCompany",
            "UserPaymentServiceProvider",
            "UserApiKey",
        ]
        .iter()
        .find_map(|k| find(&v, k)["id"].as_i64())
        .unwrap_or(0);
        if session.is_empty() || user_id == 0 {
            return Err(CoreError::Protocol {
                message: "bunq session: missing token or user".into(),
            }
            .into());
        }
        self.state.session_token = session.to_string();
        self.state.user_id = user_id;
        Ok(())
    }

    /// Reuses the stored session if it is still valid.
    pub(super) async fn session_alive(&mut self) -> bool {
        if self.state.session_token.is_empty() || self.state.user_id == 0 {
            return false;
        }
        let path = format!("/v1/user/{}", self.state.user_id);
        self.get(&path).await.is_ok()
    }
}

pub(super) fn key_fingerprint(api_key: &str) -> String {
    hex::encode(&Sha256::digest(api_key.as_bytes())[..8])
}

/// A path that stays on the bunq host when appended to the base URL: starts
/// with `/v1/`, no userinfo (`@`), no scheme or protocol-relative part, no
/// backslashes, whitespace or control characters.
pub(super) fn is_api_path(path: &str) -> bool {
    path.starts_with("/v1/")
        && !path.contains('@')
        && !path.contains("//")
        && !path.contains('\\')
        && !path.chars().any(|c| c.is_whitespace() || c.is_control())
}
