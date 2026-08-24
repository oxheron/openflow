use serde::Serialize;
use unicode_segmentation::UnicodeSegmentation;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[allow(dead_code)]
pub enum PlatformName {
    Macos,
    Linux,
    Unsupported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[allow(dead_code)]
pub enum SessionType {
    Native,
    X11,
    Wayland,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[allow(dead_code)]
pub enum TargetPolicy {
    Direct,
    OverlayClipboard,
    Blocked,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlatformCapabilities {
    pub platform: PlatformName,
    pub session_type: SessionType,
    pub direct_insertion_available: bool,
    pub target_verification_available: bool,
    pub policy: TargetPolicy,
    pub reason: String,
}

#[doc(hidden)]
/// Half-open character range in a platform-native offset convention.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextRange {
    /// Inclusive native character offset.
    pub start: usize,
    /// Exclusive native character offset.
    pub end: usize,
}

#[doc(hidden)]
/// Character-offset convention exposed by a native accessibility provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OffsetUnit {
    /// AT-SPI Unicode scalar offsets.
    #[cfg_attr(target_os = "macos", allow(dead_code))]
    UnicodeScalar,
    /// macOS Accessibility UTF-16 offsets.
    #[cfg_attr(target_os = "linux", allow(dead_code))]
    Utf16,
}

#[doc(hidden)]
/// Planned, but not yet committed, native text mutation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MutationPlan {
    /// Native range replaced by the operation.
    pub range: TextRange,
    /// Replacement text sent to the provider.
    pub replacement: String,
    /// Expected complete field value after the operation.
    pub next_value: String,
    /// Expected native selection after the operation.
    pub next_selection: TextRange,
    next_span_end: usize,
}

/// Exact value, selection, and OpenFlow-owned range expected in a native text
/// target. Native adapters only commit a plan after verifying its postcondition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackedText {
    expected_value: String,
    expected_selection: TextRange,
    span_start: usize,
    span_end: usize,
    unit: OffsetUnit,
}

impl TrackedText {
    /// Creates a tracker for an exact field value and selection.
    pub fn new(value: String, selection: TextRange, unit: OffsetUnit) -> Result<Self, String> {
        let length = text_len(&value, unit);
        if selection.start > selection.end || selection.end > length {
            return Err("the accessibility target returned an invalid selection".into());
        }
        Ok(Self {
            expected_value: value,
            expected_selection: selection,
            span_start: selection.start,
            span_end: selection.end,
            unit,
        })
    }

    /// Verifies that neither the field value nor selection changed externally.
    pub fn verify(&self, value: &str, selection: TextRange) -> Result<(), String> {
        if value != self.expected_value {
            return Err("the target value changed outside OpenFlow".into());
        }
        if selection != self.expected_selection {
            return Err("the target selection or caret moved outside OpenFlow".into());
        }
        Ok(())
    }

    #[cfg(target_os = "macos")]
    /// Returns the complete expected field value.
    pub fn expected_value(&self) -> &str {
        &self.expected_value
    }

    /// Plans an append or initial selection replacement.
    pub fn plan_insert(&self, text: &str) -> Result<MutationPlan, String> {
        self.plan(self.expected_selection, text)
    }

    /// Plans a correction addressed by transcript grapheme indices.
    pub fn plan_patch(
        &self,
        shadow: &str,
        start_grapheme: usize,
        end_grapheme: usize,
        replacement: &str,
    ) -> Result<MutationPlan, String> {
        if text_len(shadow, self.unit) != self.span_end.saturating_sub(self.span_start) {
            return Err("the tracked native range no longer matches the transcript".into());
        }
        let relative = grapheme_range(shadow, start_grapheme, end_grapheme, self.unit)?;
        let start = self
            .span_start
            .checked_add(relative.start)
            .ok_or_else(|| "target range overflow".to_string())?;
        let end = self
            .span_start
            .checked_add(relative.end)
            .ok_or_else(|| "target range overflow".to_string())?;
        self.plan(TextRange { start, end }, replacement)
    }

    fn plan(&self, range: TextRange, replacement: &str) -> Result<MutationPlan, String> {
        let next_value = replace_native_range(&self.expected_value, range, replacement, self.unit)?;
        let removed = range.end.saturating_sub(range.start);
        let inserted = text_len(replacement, self.unit);
        let next_span_end = self
            .span_end
            .checked_sub(removed)
            .and_then(|value| value.checked_add(inserted))
            .ok_or_else(|| "target range overflow".to_string())?;
        let caret = range
            .start
            .checked_add(inserted)
            .ok_or_else(|| "target range overflow".to_string())?;
        Ok(MutationPlan {
            range,
            replacement: replacement.into(),
            next_value,
            next_selection: TextRange {
                start: caret,
                end: caret,
            },
            next_span_end,
        })
    }

    /// Commits a mutation after its native postcondition was verified.
    pub fn commit(&mut self, plan: MutationPlan) {
        self.expected_value = plan.next_value;
        self.expected_selection = plan.next_selection;
        self.span_end = plan.next_span_end;
    }
}

fn text_len(value: &str, unit: OffsetUnit) -> usize {
    match unit {
        OffsetUnit::UnicodeScalar => value.chars().count(),
        OffsetUnit::Utf16 => value.encode_utf16().count(),
    }
}

fn grapheme_range(
    value: &str,
    start: usize,
    end: usize,
    unit: OffsetUnit,
) -> Result<TextRange, String> {
    let graphemes: Vec<&str> = value.graphemes(true).collect();
    if start > end || end > graphemes.len() {
        return Err("patch range is outside the tracked transcript".into());
    }
    Ok(TextRange {
        start: graphemes[..start]
            .iter()
            .map(|part| text_len(part, unit))
            .sum(),
        end: graphemes[..end]
            .iter()
            .map(|part| text_len(part, unit))
            .sum(),
    })
}

fn replace_native_range(
    value: &str,
    range: TextRange,
    replacement: &str,
    unit: OffsetUnit,
) -> Result<String, String> {
    if range.start > range.end {
        return Err("target range is reversed".into());
    }
    let mut start_byte = None;
    let mut end_byte = None;
    let mut offset = 0;
    for (byte, character) in value.char_indices() {
        if offset == range.start {
            start_byte = Some(byte);
        }
        if offset == range.end {
            end_byte = Some(byte);
            break;
        }
        offset += match unit {
            OffsetUnit::UnicodeScalar => 1,
            OffsetUnit::Utf16 => character.len_utf16(),
        };
        if offset > range.end {
            return Err("target range splits a Unicode character".into());
        }
    }
    if offset == range.start {
        start_byte.get_or_insert(value.len());
    }
    if offset == range.end {
        end_byte.get_or_insert(value.len());
    }
    let (start_byte, end_byte) = start_byte
        .zip(end_byte)
        .ok_or_else(|| "target range is outside the field value".to_string())?;
    let mut result = String::with_capacity(value.len() + replacement.len());
    result.push_str(&value[..start_byte]);
    result.push_str(replacement);
    result.push_str(&value[end_byte..]);
    Ok(result)
}

pub fn capabilities() -> PlatformCapabilities {
    #[cfg(target_os = "macos")]
    return macos::capabilities();
    #[cfg(target_os = "linux")]
    return linux::capabilities();
    #[allow(unreachable_code)]
    PlatformCapabilities {
        platform: PlatformName::Unsupported,
        session_type: SessionType::Unknown,
        direct_insertion_available: false,
        target_verification_available: false,
        policy: TargetPolicy::OverlayClipboard,
        reason: "This platform has no verified text-target adapter; using overlay and clipboard."
            .into(),
    }
}

#[cfg(target_os = "linux")]
pub(crate) use linux::PlatformTarget;
#[cfg(target_os = "macos")]
pub(crate) use macos::PlatformTarget;

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
#[derive(Debug)]
pub(crate) struct PlatformTarget;

pub(crate) async fn capture_target() -> Result<PlatformTarget, String> {
    #[cfg(target_os = "linux")]
    return linux::capture_target().await;
    #[cfg(target_os = "macos")]
    return macos::capture_target().await;
    #[allow(unreachable_code)]
    Err(capabilities().reason)
}

pub(crate) async fn insert_text(target: &mut PlatformTarget, text: &str) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    return linux::insert_text(target, text).await;
    #[cfg(target_os = "macos")]
    return macos::insert_text(target, text).await;
    #[allow(unreachable_code)]
    {
        let _ = (target, text);
        Err(capabilities().reason)
    }
}

pub(crate) async fn replace_text(
    target: &mut PlatformTarget,
    shadow: &str,
    start_grapheme: usize,
    end_grapheme: usize,
    replacement: &str,
) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    return linux::replace_text(target, shadow, start_grapheme, end_grapheme, replacement).await;
    #[cfg(target_os = "macos")]
    return macos::replace_text(target, shadow, start_grapheme, end_grapheme, replacement).await;
    #[allow(unreachable_code)]
    {
        let _ = (target, shadow, start_grapheme, end_grapheme, replacement);
        Err(capabilities().reason)
    }
}
