use crate::error::ServerError;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use constant_time_eq::constant_time_eq;
use openflow_protocol::{DeviceInfo, PairDeviceResponse};
use rand::{RngCore, rngs::OsRng};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::sync::RwLock;
use uuid::Uuid;

#[derive(Debug, Clone)]
pub enum Principal {
    Admin,
    Device(DeviceInfo),
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct AuthStore {
    /// Present only for automatically generated local administrator tokens.
    #[serde(default)]
    admin_token_sha256: Option<String>,
    #[serde(default)]
    devices: Vec<StoredDevice>,
    #[serde(default)]
    pairing_codes: Vec<StoredPairingCode>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredDevice {
    info: DeviceInfo,
    token_sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredPairingCode {
    code_sha256: String,
    expires_at_unix_seconds: u64,
}

#[derive(Debug)]
struct AuthInner {
    path: PathBuf,
    store: RwLock<AuthStore>,
    persist_lock: tokio::sync::Mutex<()>,
    admin_token_sha256: String,
    bootstrap_admin_token: Mutex<Option<String>>,
}

#[derive(Clone, Debug)]
pub struct AuthManager {
    inner: Arc<AuthInner>,
}

impl AuthManager {
    /// Loads the persisted device store and establishes the administrator secret.
    ///
    /// # Errors
    ///
    /// Returns an error when the token is too short or the store cannot be read,
    /// decoded, or persisted.
    pub async fn load(
        path: PathBuf,
        configured_admin_token: Option<String>,
        rotate_generated_token: bool,
    ) -> Result<Self, ServerError> {
        let mut store = match tokio::fs::read(&path).await {
            Ok(bytes) => serde_json::from_slice(&bytes)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => AuthStore::default(),
            Err(error) => return Err(error.into()),
        };
        let (admin_token_sha256, bootstrap_admin_token) = match configured_admin_token {
            Some(token) if valid_configured_secret(&token) => {
                store.admin_token_sha256 = None;
                (hash_secret(&token), None)
            }
            Some(_) => {
                return Err(ServerError::Configuration(
                    "OPENFLOW_ADMIN_TOKEN must be 24-512 URL-safe characters".into(),
                ));
            }
            None if rotate_generated_token => {
                let token = random_secret();
                let digest = hash_secret(&token);
                store.admin_token_sha256 = Some(digest.clone());
                (digest, Some(token))
            }
            None => match store.admin_token_sha256.as_deref() {
                Some(digest) if digest.len() == 64 && hex::decode(digest).is_ok() => {
                    (digest.to_owned(), None)
                }
                Some(_) => {
                    return Err(ServerError::Configuration(
                        "stored administrator token hash is invalid".into(),
                    ));
                }
                None => {
                    let token = random_secret();
                    let digest = hash_secret(&token);
                    store.admin_token_sha256 = Some(digest.clone());
                    (digest, Some(token))
                }
            },
        };
        let manager = Self {
            inner: Arc::new(AuthInner {
                path,
                store: RwLock::new(store),
                persist_lock: tokio::sync::Mutex::new(()),
                admin_token_sha256,
                bootstrap_admin_token: Mutex::new(bootstrap_admin_token),
            }),
        };
        manager.persist().await?;
        Ok(manager)
    }

    /// Returns an automatically generated administrator token exactly once.
    /// Embedders should pass it to the local client over their protected IPC
    /// channel and must not write it to ordinary application logs.
    pub fn take_bootstrap_admin_token(&self) -> Option<String> {
        self.inner
            .bootstrap_admin_token
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
    }

    /// Resolves an administrator or enrolled-device bearer token.
    ///
    /// # Errors
    ///
    /// Returns [`ServerError::Unauthorized`] when the token is unknown.
    pub async fn authenticate(&self, token: &str) -> Result<Principal, ServerError> {
        let presented = hash_secret(token);
        if hashes_equal(&presented, &self.inner.admin_token_sha256) {
            return Ok(Principal::Admin);
        }
        let store = self.inner.store.read().await;
        store
            .devices
            .iter()
            .find(|device| hashes_equal(&presented, &device.token_sha256))
            .map(|device| Principal::Device(device.info.clone()))
            .ok_or(ServerError::Unauthorized)
    }

    /// Creates and persists a single-use pairing code.
    ///
    /// # Errors
    ///
    /// Returns an error when the updated store cannot be persisted.
    pub async fn create_pairing_code(&self, ttl: Duration) -> Result<(String, u64), ServerError> {
        let code = random_secret();
        let now = unix_now();
        let expires_at = now.saturating_add(ttl.as_secs());
        {
            let mut store = self.inner.store.write().await;
            store
                .pairing_codes
                .retain(|entry| entry.expires_at_unix_seconds > now);
            store.pairing_codes.push(StoredPairingCode {
                code_sha256: hash_secret(&code),
                expires_at_unix_seconds: expires_at,
            });
        }
        self.persist().await?;
        Ok((code, expires_at))
    }

    /// Exchanges a valid pairing code for a new device credential.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid device name, expired/used code, or a
    /// persistence failure.
    pub async fn pair_device(
        &self,
        pairing_code: &str,
        device_name: &str,
    ) -> Result<PairDeviceResponse, ServerError> {
        let name = device_name.trim();
        if name.is_empty() || name.len() > 128 || name.chars().any(char::is_control) {
            return Err(ServerError::BadRequest(
                "device_name must contain 1-128 printable characters".into(),
            ));
        }
        let code_hash = hash_secret(pairing_code);
        let now = unix_now();
        let token = random_secret();
        let device_id = Uuid::new_v4();
        {
            let mut store = self.inner.store.write().await;
            let code_index = store.pairing_codes.iter().position(|entry| {
                entry.expires_at_unix_seconds > now && hashes_equal(&entry.code_sha256, &code_hash)
            });
            let Some(code_index) = code_index else {
                // Do not distinguish expired, previously used, and invalid codes.
                return Err(ServerError::Unauthorized);
            };
            store.pairing_codes.remove(code_index);
            store
                .pairing_codes
                .retain(|entry| entry.expires_at_unix_seconds > now);
            store.devices.push(StoredDevice {
                info: DeviceInfo {
                    id: device_id,
                    name: name.into(),
                    created_at_unix_seconds: now,
                },
                token_sha256: hash_secret(&token),
            });
        }
        self.persist().await?;
        Ok(PairDeviceResponse {
            device_id,
            device_token: token,
        })
    }

    pub async fn list_devices(&self) -> Vec<DeviceInfo> {
        self.inner
            .store
            .read()
            .await
            .devices
            .iter()
            .map(|device| device.info.clone())
            .collect()
    }

    /// Revokes one paired device.
    ///
    /// # Errors
    ///
    /// Returns an error when the device is unknown or the store cannot be
    /// persisted.
    pub async fn revoke_device(&self, id: Uuid) -> Result<(), ServerError> {
        let removed = {
            let mut store = self.inner.store.write().await;
            let previous = store.devices.len();
            store.devices.retain(|device| device.info.id != id);
            store.devices.len() != previous
        };
        if !removed {
            return Err(ServerError::NotFound(format!("device {id}")));
        }
        self.persist().await
    }

    async fn persist(&self) -> Result<(), ServerError> {
        let _guard = self.inner.persist_lock.lock().await;
        let snapshot = self.inner.store.read().await.clone();
        persist_json_atomic(&self.inner.path, &snapshot).await
    }
}

async fn persist_json_atomic(path: &Path, value: &impl Serialize) -> Result<(), ServerError> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let bytes = serde_json::to_vec_pretty(value)?;
    let temporary = path.with_extension("json.tmp");
    tokio::fs::write(&temporary, bytes).await?;
    #[cfg(unix)]
    {
        let mut permissions = tokio::fs::metadata(&temporary).await?.permissions();
        permissions.set_mode(0o600);
        tokio::fs::set_permissions(&temporary, permissions).await?;
    }
    tokio::fs::rename(temporary, path).await?;
    Ok(())
}

fn random_secret() -> String {
    let mut bytes = [0_u8; 32];
    OsRng.fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

fn valid_configured_secret(secret: &str) -> bool {
    (24..=512).contains(&secret.len())
        && secret
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn hash_secret(secret: &str) -> String {
    hex::encode(Sha256::digest(secret.as_bytes()))
}

fn hashes_equal(left: &str, right: &str) -> bool {
    left.len() == right.len() && constant_time_eq(left.as_bytes(), right.as_bytes())
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
