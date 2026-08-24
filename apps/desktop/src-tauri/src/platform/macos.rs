use super::{
    MutationPlan, OffsetUnit, PlatformCapabilities, PlatformName, SessionType, TargetPolicy,
    TextRange, TrackedText,
};
use axuielement::{
    ax_attribute::{attributes, roles, subroles},
    prelude::{
        AXRange, AXUIElement, is_process_trusted, is_process_trusted_with_prompt, system_wide,
    },
};

const AX_CONTAINS_PROTECTED_CONTENT: &str = "AXContainsProtectedContent";
const AX_PROTECTED_CONTENT: &str = "AXProtectedContent";
const MAX_FIELD_UTF16_UNITS: usize = 1_048_576;

#[derive(Debug)]
pub(crate) struct PlatformTarget {
    element: AXUIElement,
    pid: i32,
    tracked: TrackedText,
}

pub(super) fn capabilities() -> PlatformCapabilities {
    let trusted = is_process_trusted();
    PlatformCapabilities {
        platform: PlatformName::Macos,
        session_type: SessionType::Native,
        direct_insertion_available: trusted,
        target_verification_available: trusted,
        policy: if trusted {
            TargetPolicy::Direct
        } else {
            TargetPolicy::OverlayClipboard
        },
        reason: if trusted {
            "macOS Accessibility semantic insertion is available for verified, non-secure text controls; other targets use overlay and clipboard."
        } else {
            "Grant OpenFlow Accessibility permission in System Settings to enable verified direct insertion; using overlay and clipboard for now."
        }
        .into(),
    }
}

#[allow(clippy::unused_async)]
pub(super) async fn capture_target() -> Result<PlatformTarget, String> {
    if !is_process_trusted() {
        let _ = is_process_trusted_with_prompt();
        return Err(
            "macOS Accessibility permission is required; OpenFlow kept the text in the overlay and clipboard"
                .into(),
        );
    }
    let system = system_wide()
        .ok_or_else(|| "macOS did not provide a system Accessibility element".to_string())?;
    let element = system
        .focused_ui_element()
        .map_err(|error| format!("cannot read the focused macOS control: {error}"))?
        .ok_or_else(|| "macOS reports no focused Accessibility control".to_string())?;
    element
        .set_timeout(0.75)
        .map_err(|error| format!("cannot set the Accessibility message timeout: {error}"))?;
    let pid = element
        .pid()
        .map_err(|error| format!("cannot identify the focused application: {error}"))?;
    if u32::try_from(pid).ok() == Some(std::process::id()) {
        return Err("OpenFlow never inserts directly into its own window".into());
    }
    validate_element(&element, pid)?;
    let snapshot = snapshot(&element)?;
    Ok(PlatformTarget {
        element,
        pid,
        tracked: TrackedText::new(snapshot.value, snapshot.selection, OffsetUnit::Utf16)?,
    })
}

fn validate_element(element: &AXUIElement, expected_pid: i32) -> Result<(), String> {
    let attribute_names = element
        .attribute_names()
        .map_err(|error| format!("cannot enumerate target attributes: {error}"))?;
    if element
        .pid()
        .map_err(|error| format!("the macOS target disappeared: {error}"))?
        != expected_pid
    {
        return Err("the macOS target changed applications".into());
    }
    if element
        .bool_attribute(attributes::AX_FOCUSED_ATTRIBUTE)
        .map_err(|error| format!("cannot verify target focus: {error}"))?
        != Some(true)
    {
        return Err("the captured macOS text control is no longer focused".into());
    }
    if element
        .bool_attribute(attributes::AX_ENABLED_ATTRIBUTE)
        .map_err(|error| format!("cannot verify target availability: {error}"))?
        != Some(true)
    {
        return Err("the captured macOS text control is disabled".into());
    }
    if attribute_names
        .iter()
        .any(|name| name == attributes::AX_IS_EDITABLE_ATTRIBUTE)
        && element
            .bool_attribute(attributes::AX_IS_EDITABLE_ATTRIBUTE)
            .map_err(|error| format!("cannot verify target editability: {error}"))?
            == Some(false)
    {
        return Err("the captured macOS text control is read-only".into());
    }
    let role = required_string(element, attributes::AX_ROLE_ATTRIBUTE, "role")?;
    if !matches!(
        role.as_str(),
        roles::AX_TEXT_FIELD_ROLE | roles::AX_TEXT_AREA_ROLE
    ) {
        return Err(format!(
            "the focused macOS role ({role}) is not an approved text-entry role"
        ));
    }
    let subrole = if attribute_names
        .iter()
        .any(|name| name == attributes::AX_SUBROLE_ATTRIBUTE)
    {
        element
            .string_attribute(attributes::AX_SUBROLE_ATTRIBUTE)
            .map_err(|error| format!("cannot inspect target subrole: {error}"))?
    } else {
        None
    };
    if subrole.as_deref() == Some(subroles::AX_SECURE_TEXT_FIELD_SUBROLE) {
        return Err("secure text fields are never direct-insertion targets".into());
    }
    for attribute in [AX_CONTAINS_PROTECTED_CONTENT, AX_PROTECTED_CONTENT] {
        if attribute_names.iter().any(|name| name == attribute)
            && element
                .bool_attribute(attribute)
                .map_err(|error| format!("cannot inspect protected-content state: {error}"))?
                == Some(true)
        {
            return Err("the focused control contains protected content".into());
        }
    }
    for attribute in [
        attributes::AX_SELECTED_TEXT_RANGE_ATTRIBUTE,
        attributes::AX_SELECTED_TEXT_ATTRIBUTE,
    ] {
        if !element
            .is_attribute_settable(attribute)
            .map_err(|error| format!("cannot inspect target text operations: {error}"))?
        {
            return Err(format!(
                "the focused control does not permit semantic {attribute} operations"
            ));
        }
    }
    Ok(())
}

fn required_string(
    element: &AXUIElement,
    attribute: &str,
    description: &str,
) -> Result<String, String> {
    element
        .string_attribute(attribute)
        .map_err(|error| format!("cannot read target {description}: {error}"))?
        .ok_or_else(|| format!("the focused control has no {description}"))
}

struct Snapshot {
    value: String,
    selection: TextRange,
}

fn snapshot(element: &AXUIElement) -> Result<Snapshot, String> {
    let value = required_string(element, attributes::AX_VALUE_ATTRIBUTE, "text value")?;
    if value.encode_utf16().count() > MAX_FIELD_UTF16_UNITS {
        return Err("the focused field is too large for verified insertion".into());
    }
    let range = element
        .range_attribute(attributes::AX_SELECTED_TEXT_RANGE_ATTRIBUTE)
        .map_err(|error| format!("cannot read target selection: {error}"))?
        .ok_or_else(|| "the focused control has no selected-text range".to_string())?;
    let start = usize::try_from(range.location)
        .map_err(|_| "the focused control returned a negative selection start")?;
    let length = usize::try_from(range.length)
        .map_err(|_| "the focused control returned a negative selection length")?;
    let end = start
        .checked_add(length)
        .ok_or_else(|| "the focused control returned an overflowing selection".to_string())?;
    if end > value.encode_utf16().count() {
        return Err("the focused control returned a selection outside its value".into());
    }
    Ok(Snapshot {
        value,
        selection: TextRange { start, end },
    })
}

#[allow(clippy::unused_async)]
pub(super) async fn insert_text(target: &mut PlatformTarget, text: &str) -> Result<(), String> {
    revalidate(target)?;
    let plan = target.tracked.plan_insert(text)?;
    apply(target, plan)
}

#[allow(clippy::unused_async)]
pub(super) async fn replace_text(
    target: &mut PlatformTarget,
    shadow: &str,
    start_grapheme: usize,
    end_grapheme: usize,
    replacement: &str,
) -> Result<(), String> {
    revalidate(target)?;
    let plan = target
        .tracked
        .plan_patch(shadow, start_grapheme, end_grapheme, replacement)?;
    apply(target, plan)
}

fn revalidate(target: &PlatformTarget) -> Result<(), String> {
    validate_element(&target.element, target.pid)?;
    let current = snapshot(&target.element)?;
    target.tracked.verify(&current.value, current.selection)
}

fn apply(target: &mut PlatformTarget, plan: MutationPlan) -> Result<(), String> {
    let location = isize::try_from(plan.range.start).map_err(|_| "target range is too large")?;
    let length = isize::try_from(plan.range.end.saturating_sub(plan.range.start))
        .map_err(|_| "target range is too large")?;
    let mutation_range = AXRange { location, length };
    target
        .element
        .set_range_attribute(attributes::AX_SELECTED_TEXT_RANGE_ATTRIBUTE, mutation_range)
        .map_err(|error| format!("macOS rejected the semantic target range: {error}"))?;

    // Recheck focus, value, and the newly selected exact range immediately
    // before the write. This closes the common focus-race window without using
    // keyboard-event synthesis.
    validate_element(&target.element, target.pid)?;
    let prepared = snapshot(&target.element)?;
    if prepared.value != target.tracked.expected_value() || prepared.selection != plan.range {
        return Err("the target changed while preparing the semantic text range".into());
    }
    target
        .element
        .set_string_attribute(attributes::AX_SELECTED_TEXT_ATTRIBUTE, &plan.replacement)
        .map_err(|error| format!("macOS rejected semantic text insertion: {error}"))?;
    target
        .element
        .set_range_attribute(
            attributes::AX_SELECTED_TEXT_RANGE_ATTRIBUTE,
            AXRange {
                location: isize::try_from(plan.next_selection.start)
                    .map_err(|_| "target range is too large")?,
                length: 0,
            },
        )
        .map_err(|error| format!("macOS could not restore the target caret: {error}"))?;
    let current = snapshot(&target.element)?;
    if current.value != plan.next_value || current.selection != plan.next_selection {
        return Err(
            "the target did not confirm the exact value and caret after insertion; the lease is invalid"
                .into(),
        );
    }
    target.tracked.commit(plan);
    Ok(())
}
