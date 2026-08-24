use crate::platform::{self, PlatformCapabilities, TargetPolicy};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::Mutex;
use unicode_segmentation::UnicodeSegmentation;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum TargetError {
    #[error("no text target is currently captured")]
    Missing,
    #[error("the captured text target no longer matches this lease")]
    StaleLease,
    #[error("the target revision changed")]
    StaleRevision,
    #[error("the text in the target changed outside OpenFlow")]
    TextChanged,
    #[error("patch range is outside the tracked text")]
    InvalidRange,
    #[error("direct insertion is unavailable: {0}")]
    Unsupported(String),
}

#[derive(Debug)]
struct ActiveTarget {
    lease_id: u64,
    revision: u64,
    shadow_text: String,
    policy: TargetPolicy,
    platform_target: Option<platform::PlatformTarget>,
}

#[derive(Debug, Default)]
struct InnerState {
    next_lease_id: u64,
    active: Option<ActiveTarget>,
}

#[derive(Debug, Default)]
pub struct TargetState(Mutex<InnerState>);

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TargetLease {
    lease_id: u64,
    policy: TargetPolicy,
    initial_revision: u64,
    reason: String,
}

impl TargetLease {
    #[doc(hidden)]
    /// Returns the opaque ID used to release this target lease.
    pub fn lease_id(&self) -> u64 {
        self.lease_id
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StableInsertRequest {
    lease_id: u64,
    base_revision: u64,
    expected_prefix: String,
    text: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PatchRequest {
    lease_id: u64,
    base_revision: u64,
    expected_text: String,
    start_grapheme: usize,
    end_grapheme: usize,
    replacement: String,
}

impl TargetState {
    pub async fn capture(&self) -> Result<TargetLease, TargetError> {
        let capabilities = platform::capabilities();
        let capture = platform::capture_target().await;
        let (platform_target, policy, reason) = match capture {
            Ok(target) => (
                Some(target),
                TargetPolicy::Direct,
                "Captured a verified, focused editable text target.".into(),
            ),
            Err(reason) => (None, TargetPolicy::OverlayClipboard, reason),
        };
        let mut state = self.0.lock().await;
        state.next_lease_id = state.next_lease_id.saturating_add(1);
        let lease_id = state.next_lease_id;
        state.active = Some(ActiveTarget {
            lease_id,
            revision: 0,
            shadow_text: String::new(),
            policy,
            platform_target,
        });
        Ok(TargetLease {
            lease_id,
            policy,
            initial_revision: 0,
            reason: if policy == TargetPolicy::Direct {
                reason
            } else if capabilities.policy == TargetPolicy::Direct {
                format!("{reason} Direct insertion failed closed for this target.")
            } else {
                reason
            },
        })
    }

    pub async fn insert(&self, request: &StableInsertRequest) -> Result<u64, TargetError> {
        let mut state = self.0.lock().await;
        let target = state.active.as_mut().ok_or(TargetError::Missing)?;
        verify(
            target,
            request.lease_id,
            request.base_revision,
            &request.expected_prefix,
        )?;
        if target.policy != TargetPolicy::Direct {
            return Err(TargetError::Unsupported(platform::capabilities().reason));
        }
        let platform_target = target.platform_target.as_mut().ok_or_else(|| {
            TargetError::Unsupported("the verified native target is unavailable".into())
        })?;
        if let Err(error) = platform::insert_text(platform_target, &request.text).await {
            // A native mutation may have partially succeeded before an
            // accessibility provider failed. Retrying against the old shadow
            // could duplicate text, so invalidate the lease immediately.
            state.active = None;
            return Err(TargetError::Unsupported(error));
        }
        let target = state.active.as_mut().ok_or(TargetError::Missing)?;
        target.shadow_text.push_str(&request.text);
        target.revision = target.revision.saturating_add(1);
        Ok(target.revision)
    }

    pub async fn apply_patch(&self, request: &PatchRequest) -> Result<u64, TargetError> {
        let mut state = self.0.lock().await;
        let target = state.active.as_mut().ok_or(TargetError::Missing)?;
        verify(
            target,
            request.lease_id,
            request.base_revision,
            &request.expected_text,
        )?;
        let updated = replace_graphemes(
            &target.shadow_text,
            request.start_grapheme,
            request.end_grapheme,
            &request.replacement,
        )?;
        if target.policy != TargetPolicy::Direct {
            return Err(TargetError::Unsupported(platform::capabilities().reason));
        }
        let shadow = target.shadow_text.clone();
        let platform_target = target.platform_target.as_mut().ok_or_else(|| {
            TargetError::Unsupported("the verified native target is unavailable".into())
        })?;
        if let Err(error) = platform::replace_text(
            platform_target,
            &shadow,
            request.start_grapheme,
            request.end_grapheme,
            &request.replacement,
        )
        .await
        {
            state.active = None;
            return Err(TargetError::Unsupported(error));
        }
        let target = state.active.as_mut().ok_or(TargetError::Missing)?;
        target.shadow_text = updated;
        target.revision = target.revision.saturating_add(1);
        Ok(target.revision)
    }

    pub async fn release(&self, lease_id: u64) -> Result<(), TargetError> {
        let mut state = self.0.lock().await;
        match &state.active {
            Some(target) if target.lease_id == lease_id => {
                state.active = None;
                Ok(())
            }
            Some(_) => Err(TargetError::StaleLease),
            None => Ok(()),
        }
    }
}

fn verify(
    target: &ActiveTarget,
    lease_id: u64,
    revision: u64,
    expected: &str,
) -> Result<(), TargetError> {
    if target.lease_id != lease_id {
        return Err(TargetError::StaleLease);
    }
    if target.revision != revision {
        return Err(TargetError::StaleRevision);
    }
    if target.shadow_text != expected {
        return Err(TargetError::TextChanged);
    }
    Ok(())
}

pub fn replace_graphemes(
    value: &str,
    start: usize,
    end: usize,
    replacement: &str,
) -> Result<String, TargetError> {
    let graphemes: Vec<&str> = value.graphemes(true).collect();
    if start > end || end > graphemes.len() {
        return Err(TargetError::InvalidRange);
    }
    let mut updated = String::with_capacity(value.len() + replacement.len());
    updated.push_str(&graphemes[..start].concat());
    updated.push_str(replacement);
    updated.push_str(&graphemes[end..].concat());
    Ok(updated)
}

pub fn capabilities() -> PlatformCapabilities {
    platform::capabilities()
}
