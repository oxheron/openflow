use keyring::v1::{Entry, Error as KeyringError};
use url::Url;

const SERVICE: &str = "org.openflow.desktop.server";

#[tauri::command]
pub async fn load_server_credential(endpoint: String) -> Result<Option<String>, String> {
    let account = endpoint_account(&endpoint)?;
    tokio::task::spawn_blocking(move || {
        let entry = Entry::new(SERVICE, &account).map_err(credential_error)?;
        match entry.get_password() {
            Ok(token) if valid_token(&token) => Ok(Some(token)),
            Ok(_) => {
                Err("the stored OpenFlow credential is malformed; remove and pair it again".into())
            }
            Err(KeyringError::NoEntry) => Ok(None),
            Err(error) => Err(credential_error(error)),
        }
    })
    .await
    .map_err(|error| format!("credential task failed: {error}"))?
}

#[tauri::command]
pub async fn store_server_credential(endpoint: String, token: String) -> Result<(), String> {
    let account = endpoint_account(&endpoint)?;
    let token = token.trim().to_owned();
    if !valid_token(&token) {
        return Err(
            "server credentials must contain 16-512 URL-safe letters, numbers, _ or -".into(),
        );
    }
    tokio::task::spawn_blocking(move || {
        Entry::new(SERVICE, &account)
            .map_err(credential_error)?
            .set_password(&token)
            .map_err(credential_error)
    })
    .await
    .map_err(|error| format!("credential task failed: {error}"))?
}

#[tauri::command]
pub async fn delete_server_credential(endpoint: String) -> Result<(), String> {
    let account = endpoint_account(&endpoint)?;
    tokio::task::spawn_blocking(move || {
        let entry = Entry::new(SERVICE, &account).map_err(credential_error)?;
        match entry.delete_credential() {
            Ok(()) | Err(KeyringError::NoEntry) => Ok(()),
            Err(error) => Err(credential_error(error)),
        }
    })
    .await
    .map_err(|error| format!("credential task failed: {error}"))?
}

#[doc(hidden)]
/// Normalizes a server endpoint into its credential-store account.
pub fn endpoint_account(endpoint: &str) -> Result<String, String> {
    let mut url = Url::parse(endpoint.trim()).map_err(|_| "server URL is invalid".to_owned())?;
    if !url.username().is_empty() || url.password().is_some() {
        return Err("server URLs must not contain embedded credentials".into());
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err("server URLs must not contain a query or fragment".into());
    }
    let secure = url.scheme() == "https";
    let loopback_http = url.scheme() == "http"
        && url.host_str().is_some_and(|host| {
            host == "localhost"
                || host
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback())
        });
    if !secure && !loopback_http {
        return Err("remote server credentials require HTTPS".into());
    }
    url.set_fragment(None);
    let normalized = url.as_str().trim_end_matches('/');
    if normalized.is_empty() || normalized.len() > 480 {
        return Err("server URL is too long for the credential store".into());
    }
    Ok(format!("server:{normalized}"))
}

#[doc(hidden)]
/// Checks the credential grammar accepted by the native keyring boundary.
pub fn valid_token(token: &str) -> bool {
    (16..=512).contains(&token.len())
        && token
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn credential_error(error: KeyringError) -> String {
    match error {
        KeyringError::NoDefaultStore => {
            "no OS credential store is available; start/unlock Keychain or Secret Service".into()
        }
        KeyringError::NoStorageAccess(_) => {
            "the OS credential store is locked or denied access".into()
        }
        other => format!("OS credential store error: {other}"),
    }
}
