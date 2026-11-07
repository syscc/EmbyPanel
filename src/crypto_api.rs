use std::{collections::HashMap, sync::Arc};

use aes_gcm::{
    Aes256Gcm, Nonce,
    aead::{Aead, KeyInit},
};
use axum::{
    Json,
    extract::State,
    http::header,
    response::{IntoResponse, Response},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use rsa::{
    Oaep, RsaPrivateKey, RsaPublicKey,
    pkcs8::{EncodePublicKey, LineEnding},
    rand_core::OsRng,
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
use sha2::Sha256;

use crate::{
    AppState,
    error::{AppError, AppResult},
};

#[derive(Clone)]
pub struct CryptoKeys {
    private_key: Arc<RsaPrivateKey>,
    public_key_pem: Arc<String>,
}

impl CryptoKeys {
    pub fn generate() -> AppResult<Self> {
        let mut rng = OsRng;
        let private_key = RsaPrivateKey::new(&mut rng, 2048)
            .map_err(|err| AppError::Internal(format!("failed to generate RSA key: {err}")))?;
        let public_key = RsaPublicKey::from(&private_key);
        let public_key_pem = public_key
            .to_public_key_pem(LineEnding::LF)
            .map_err(|err| AppError::Internal(format!("failed to encode public key: {err}")))?;
        Ok(Self {
            private_key: Arc::new(private_key),
            public_key_pem: Arc::new(public_key_pem),
        })
    }

    pub fn decrypt_fields(&self, request: &EncryptedRequest) -> AppResult<HashMap<String, Value>> {
        let encrypted_key = decode_b64(&request.encrypted_key)?;
        let aes_key = self
            .private_key
            .decrypt(Oaep::new::<Sha256>(), &encrypted_key)
            .map_err(|err| AppError::Validation(format!("failed to decrypt request key: {err}")))?;
        if aes_key.len() != 32 {
            return Err(AppError::Validation(
                "encrypted request key must decrypt to 32 bytes".to_string(),
            ));
        }

        let cipher = Aes256Gcm::new_from_slice(&aes_key)
            .map_err(|err| AppError::Validation(format!("invalid request key: {err}")))?;
        let mut out = HashMap::new();
        for field in request.fields.values() {
            let iv = decode_b64(&field.iv)?;
            if iv.len() != 12 {
                return Err(AppError::Validation(
                    "encrypted field iv must be 12 bytes".to_string(),
                ));
            }
            let ciphertext = decode_b64(&field.value)?;
            let plaintext = cipher
                .decrypt(Nonce::from_slice(&iv), ciphertext.as_ref())
                .map_err(|_| {
                    AppError::Validation("failed to decrypt encrypted field".to_string())
                })?;
            let field: PlainField = serde_json::from_slice(&plaintext)
                .map_err(|_| AppError::Validation("invalid encrypted payload".to_string()))?;
            out.insert(field.name, field.value);
        }
        Ok(out)
    }

    pub fn decrypt_named<T: DeserializeOwned>(
        &self,
        request: &EncryptedRequest,
        name: &str,
    ) -> AppResult<T> {
        let mut fields = self.decrypt_fields(request)?;
        let value = fields
            .remove(name)
            .ok_or_else(|| AppError::Validation(format!("missing encrypted field `{name}`")))?;
        serde_json::from_value(value)
            .map_err(|_| AppError::Validation("invalid encrypted payload".to_string()))
    }
}

#[derive(Debug, Serialize)]
pub struct PublicKeyResponse {
    pub algorithm: &'static str,
    pub public_key_pem: String,
}

#[derive(Debug, Deserialize)]
pub struct EncryptedRequest {
    pub encrypted_key: String,
    pub fields: HashMap<String, EncryptedField>,
}

#[derive(Debug, Deserialize)]
pub struct EncryptedField {
    pub iv: String,
    pub value: String,
}

#[derive(Debug, Deserialize)]
struct PlainField {
    name: String,
    value: Value,
}

pub async fn public_key(State(state): State<AppState>) -> Response {
    (
        [(header::CACHE_CONTROL, "no-store")],
        Json(PublicKeyResponse {
            algorithm: "RSA-OAEP-256/AES-256-GCM",
            public_key_pem: (*state.crypto_keys.public_key_pem).clone(),
        }),
    )
        .into_response()
}

fn decode_b64(value: &str) -> AppResult<Vec<u8>> {
    URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|err| AppError::Validation(format!("invalid base64url value: {err}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Deserialize)]
    struct PasswordPayload {
        #[allow(dead_code)]
        password: String,
    }

    #[tokio::test]
    async fn encrypted_type_error_does_not_leak_canary_to_response_log_or_audit() {
        let keys = CryptoKeys::generate().unwrap();
        let aes_key = [7u8; 32];
        let iv = [3u8; 12];
        let plaintext = br#"{"name":"password","value":{"password":982764321098765432}}"#;
        let cipher = Aes256Gcm::new_from_slice(&aes_key).unwrap();
        let ciphertext = cipher
            .encrypt(Nonce::from_slice(&iv), plaintext.as_ref())
            .unwrap();
        let encrypted_key = RsaPublicKey::from(keys.private_key.as_ref())
            .encrypt(&mut OsRng, Oaep::new::<Sha256>(), &aes_key)
            .unwrap();
        let request = EncryptedRequest {
            encrypted_key: URL_SAFE_NO_PAD.encode(encrypted_key),
            fields: HashMap::from([(
                "field".into(),
                EncryptedField {
                    iv: URL_SAFE_NO_PAD.encode(iv),
                    value: URL_SAFE_NO_PAD.encode(ciphertext),
                },
            )]),
        };
        let error = keys
            .decrypt_named::<PasswordPayload>(&request, "password")
            .err()
            .unwrap();
        assert!(matches!(error, AppError::Validation(_)));
        let audit_summary = error.safe_log_message();
        assert_eq!(audit_summary, "invalid encrypted payload");
        assert!(!format!("{error:?}").contains("982764321098765432"));
        let response = error.into_response();
        assert_eq!(response.status(), axum::http::StatusCode::BAD_REQUEST);
        let body = axum::body::to_bytes(response.into_body(), 1024)
            .await
            .unwrap();
        assert!(!String::from_utf8_lossy(&body).contains("982764321098765432"));
        // Exercise the same safe summary that password-reset failure audits persist.
        let store = crate::db::SettingsStore::open_for_test(std::env::temp_dir().join(format!(
            "embypanel-crypto-canary-{}.sqlite",
            rand::random::<u64>()
        )))
        .unwrap();
        store
            .record_audit(None, "test.password.failure", &audit_summary, "failed")
            .unwrap();
        assert!(
            !serde_json::to_string(&store.list_audit_logs(None, None, 10).unwrap())
                .unwrap()
                .contains("982764321098765432")
        );
    }
}
