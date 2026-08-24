use crate::error::ServerError;
use async_trait::async_trait;
use std::{
    io::{self, IsTerminal, Write},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

/// Confirms an unauthenticated device enrollment request with the operator.
#[async_trait]
pub trait PairingPrompt: Send + Sync {
    /// Returns `true` only after the operator explicitly approves the request.
    async fn confirm(
        &self,
        device_name: &str,
        verification_code: &str,
    ) -> Result<bool, ServerError>;
}

/// A single-flight prompt attached to the server process's controlling terminal.
#[derive(Clone, Debug)]
pub struct TerminalPairingPrompt {
    enabled: bool,
    occupied: Arc<AtomicBool>,
}

impl TerminalPairingPrompt {
    #[must_use]
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            occupied: Arc::new(AtomicBool::new(false)),
        }
    }
}

#[async_trait]
impl PairingPrompt for TerminalPairingPrompt {
    async fn confirm(
        &self,
        device_name: &str,
        verification_code: &str,
    ) -> Result<bool, ServerError> {
        if !self.enabled {
            return Err(ServerError::ServiceUnavailable(
                "interactive pairing is disabled; use an administrator-created pairing code".into(),
            ));
        }
        if !io::stdin().is_terminal() || !io::stderr().is_terminal() {
            return Err(ServerError::ServiceUnavailable(
                "interactive pairing requires the server to run in a foreground terminal".into(),
            ));
        }
        self.occupied
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| {
                ServerError::Conflict("another interactive pairing request is pending".into())
            })?;

        let occupied = Arc::clone(&self.occupied);
        let device_name = device_name.to_owned();
        let verification_code = verification_code.to_owned();
        tokio::task::spawn_blocking(move || {
            let _lease = PromptLease(occupied);
            eprintln!();
            eprintln!("OpenFlow pairing request from: {device_name}");
            eprintln!("Verification code: {verification_code}");
            eprint!("Approve this device? [y/N]: ");
            io::stderr().flush()?;

            let mut response = String::new();
            io::stdin().read_line(&mut response)?;
            Ok(matches!(
                response.trim().to_ascii_lowercase().as_str(),
                "y" | "yes"
            ))
        })
        .await
        .map_err(|error| {
            ServerError::ServiceUnavailable(format!("pairing prompt task failed: {error}"))
        })?
        .map_err(ServerError::Io)
    }
}

#[derive(Debug)]
struct PromptLease(Arc<AtomicBool>);

impl Drop for PromptLease {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}
