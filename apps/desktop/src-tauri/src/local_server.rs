use serde::{Deserialize, Serialize};
use std::{net::SocketAddr, path::PathBuf, process::Stdio, time::Duration};
use tauri::State;
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::TcpStream,
    process::{Child, Command},
    sync::Mutex,
    time::{Instant, sleep, timeout},
};

const LOCAL_ADDRESS: &str = "127.0.0.1:8765";
// Startup re-verifies cached model hashes before binding, which can take a
// while on a workstation with several multi-gigabyte models on slower storage.
const START_TIMEOUT: Duration = Duration::from_mins(6);
const LOCAL_PROTOCOL_VERSION: u16 = 1;
const MAX_HEALTH_RESPONSE_BYTES: u64 = 16 * 1024;

/// Owns a local server launched by this desktop process. `kill_on_drop` keeps
/// this child tied to the client lifetime, while an independently managed
/// service found on the same port is never adopted or terminated.
#[derive(Debug, Default)]
pub struct LocalServerState {
    child: Mutex<Option<Child>>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalServerLaunch {
    available: bool,
    started: bool,
    admin_token: Option<String>,
}

#[derive(Debug, Deserialize)]
struct BootstrapMessage {
    event: String,
    admin_token: String,
}

#[tauri::command]
pub async fn ensure_local_server(
    state: State<'_, LocalServerState>,
) -> Result<LocalServerLaunch, String> {
    let address = local_address()?;
    if probe_openflow(address).await? {
        return Ok(LocalServerLaunch {
            available: true,
            started: false,
            admin_token: None,
        });
    }
    if TcpStream::connect(address).await.is_ok() {
        return Err(format!(
            "{LOCAL_ADDRESS} is occupied by a service that is not a compatible OpenFlow server"
        ));
    }

    let mut owned_child = state.child.lock().await;
    if let Some(child) = owned_child.as_mut() {
        match child.try_wait() {
            Ok(None) => {
                wait_for_child(child, address).await?;
                return Ok(LocalServerLaunch {
                    available: true,
                    started: false,
                    admin_token: None,
                });
            }
            Ok(Some(_)) => *owned_child = None,
            Err(error) => {
                return Err(format!(
                    "could not inspect the local OpenFlow server: {error}"
                ));
            }
        }
    }

    let executable = sibling_server()?;
    let mut child = Command::new(&executable)
        .env("OPENFLOW_BIND", LOCAL_ADDRESS)
        // Desktop-managed children rotate this recoverable local bootstrap;
        // service and remote deployments retain stable configured credentials.
        .env("OPENFLOW_ROTATE_BOOTSTRAP_ADMIN_TOKEN", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| format!("could not start {}: {error}", executable.display()))?;
    let stdout = child.stdout.take().ok_or_else(|| {
        "the local OpenFlow server did not expose its bootstrap channel".to_owned()
    })?;
    let mut lines = BufReader::new(stdout).lines();

    // A desktop-managed server emits a freshly rotated one-time bootstrap
    // credential before listening. Read and readiness checks race so startup
    // failures and independently managed configurations still surface promptly.
    tokio::select! {
        line = lines.next_line() => {
            let line = line
                .map_err(|error| format!("could not read the local bootstrap channel: {error}"))?
                .ok_or_else(|| "the local OpenFlow server exited before becoming ready".to_owned())?;
            let token = parse_bootstrap_token(&line)
                .ok_or_else(|| "the local OpenFlow server returned an invalid bootstrap message".to_owned())?;
            // Return the one-time token immediately so the renderer can persist
            // it even if binding fails. Its settings update invokes this command
            // again, where the owned-child path waits for readiness or reports
            // an early process exit.
            *owned_child = Some(child);
            return Ok(LocalServerLaunch {
                available: false,
                started: true,
                admin_token: Some(token),
            });
        }
        ready = wait_until_available(address) => {
            ready?;
        }
    }

    *owned_child = Some(child);
    Ok(LocalServerLaunch {
        available: true,
        started: true,
        admin_token: None,
    })
}

fn local_address() -> Result<SocketAddr, String> {
    LOCAL_ADDRESS
        .parse()
        .map_err(|error| format!("invalid built-in local server address: {error}"))
}

fn sibling_server() -> Result<PathBuf, String> {
    let name = if cfg!(windows) {
        "openflow-server.exe"
    } else {
        "openflow-server"
    };
    let executable = std::env::current_exe()
        .map_err(|error| format!("could not locate the desktop executable: {error}"))?
        .parent()
        .ok_or_else(|| "the desktop executable has no parent directory".to_owned())?
        .join(name);
    if !executable.is_file() {
        return Err(format!(
            "local server binary is not installed beside the desktop application: {}",
            executable.display()
        ));
    }
    Ok(executable)
}

async fn wait_until_available(address: SocketAddr) -> Result<(), String> {
    let deadline = Instant::now() + START_TIMEOUT;
    while Instant::now() < deadline {
        if probe_openflow(address).await? {
            return Ok(());
        }
        sleep(Duration::from_millis(100)).await;
    }
    Err("the local OpenFlow server did not start within six minutes".into())
}

async fn wait_for_child(child: &mut Child, address: SocketAddr) -> Result<(), String> {
    let deadline = Instant::now() + START_TIMEOUT;
    while Instant::now() < deadline {
        if probe_openflow(address).await? {
            return Ok(());
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                return Err(format!(
                    "the local OpenFlow server exited before becoming ready ({status})"
                ));
            }
            Ok(None) => {}
            Err(error) => {
                return Err(format!(
                    "could not inspect the local OpenFlow server: {error}"
                ));
            }
        }
        sleep(Duration::from_millis(100)).await;
    }
    Err("the local OpenFlow server did not start within six minutes".into())
}

async fn probe_openflow(address: SocketAddr) -> Result<bool, String> {
    timeout(Duration::from_millis(500), probe_openflow_inner(address))
        .await
        .unwrap_or(Ok(false))
}

async fn probe_openflow_inner(address: SocketAddr) -> Result<bool, String> {
    let Ok(mut stream) = TcpStream::connect(address).await else {
        return Ok(false);
    };
    stream
        .write_all(b"GET /health HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
        .await
        .map_err(|error| format!("could not probe the local OpenFlow server: {error}"))?;
    let mut response = Vec::new();
    stream
        .take(MAX_HEALTH_RESPONSE_BYTES)
        .read_to_end(&mut response)
        .await
        .map_err(|error| format!("could not read the local OpenFlow health response: {error}"))?;
    Ok(parse_health_response(&response))
}

#[derive(Deserialize)]
struct LocalHealth {
    status: String,
    protocol_version: u16,
}

#[doc(hidden)]
/// Validates a bounded HTTP health response from the sibling server.
#[must_use]
pub fn parse_health_response(response: &[u8]) -> bool {
    let Some(boundary) = response.windows(4).position(|window| window == b"\r\n\r\n") else {
        return false;
    };
    let headers = &response[..boundary];
    if !headers.starts_with(b"HTTP/1.1 200 ") && !headers.starts_with(b"HTTP/1.0 200 ") {
        return false;
    }
    serde_json::from_slice::<LocalHealth>(&response[boundary + 4..]).is_ok_and(|health| {
        health.status == "ok" && health.protocol_version == LOCAL_PROTOCOL_VERSION
    })
}

#[doc(hidden)]
/// Parses the server's one-time local bootstrap envelope.
pub fn parse_bootstrap_token(line: &str) -> Option<String> {
    let message: BootstrapMessage = serde_json::from_str(line).ok()?;
    (message.event == "bootstrap"
        && (24..=512).contains(&message.admin_token.len())
        && message
            .admin_token
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')))
    .then_some(message.admin_token)
}
