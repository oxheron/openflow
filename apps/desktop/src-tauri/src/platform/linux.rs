use super::{
    MutationPlan, OffsetUnit, PlatformCapabilities, PlatformName, SessionType, TargetPolicy,
    TextRange, TrackedText,
};
use atspi::{
    AccessibilityConnection, Interface, ObjectRefOwned, Role, State,
    proxy::{accessible::ObjectRefExt, editable_text::EditableTextProxy, text::TextProxy},
    zbus::{self, proxy::CacheProperties},
};
use std::{collections::VecDeque, time::Duration};

const CAPTURE_TIMEOUT: Duration = Duration::from_secs(2);
const MUTATION_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_VISITED_NODES: usize = 4_096;
const MAX_FIELD_CHARACTERS: i32 = 1_048_576;

#[derive(Debug)]
pub(crate) struct PlatformTarget {
    connection: AccessibilityConnection,
    object: ObjectRefOwned,
    tracked: TrackedText,
}

pub(super) fn capabilities() -> PlatformCapabilities {
    let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some();
    let x11 = std::env::var_os("DISPLAY").is_some();
    let session_type = if wayland {
        SessionType::Wayland
    } else if x11 {
        SessionType::X11
    } else {
        SessionType::Unknown
    };
    let available = wayland || x11;
    PlatformCapabilities {
        platform: PlatformName::Linux,
        session_type,
        direct_insertion_available: available,
        target_verification_available: available,
        policy: if available {
            TargetPolicy::Direct
        } else {
            TargetPolicy::OverlayClipboard
        },
        reason: if available {
            "AT-SPI semantic insertion is available when the focused control exposes a non-protected EditableText target; otherwise OpenFlow falls back to overlay and clipboard."
        } else {
            "No Linux graphical session was detected; using overlay and clipboard."
        }
        .into(),
    }
}

pub(super) async fn capture_target() -> Result<PlatformTarget, String> {
    if !capabilities().direct_insertion_available {
        return Err(capabilities().reason);
    }
    tokio::time::timeout(CAPTURE_TIMEOUT, capture_inner())
        .await
        .map_err(|_| "AT-SPI did not identify the focused control within two seconds".to_string())?
}

async fn capture_inner() -> Result<PlatformTarget, String> {
    let connection = AccessibilityConnection::new()
        .await
        .map_err(|error| format!("the AT-SPI accessibility bus is unavailable: {error}"))?;
    let registry = connection
        .root_accessible_on_registry()
        .await
        .map_err(|error| format!("cannot query the AT-SPI registry: {error}"))?;
    let applications = registry
        .get_children()
        .await
        .map_err(|error| format!("cannot enumerate AT-SPI applications: {error}"))?;

    // Search active top-level windows first. This avoids walking every inactive
    // application's complete accessibility tree on a busy desktop.
    for application in &applications {
        let Ok(application_proxy) = application
            .as_accessible_proxy(connection.connection())
            .await
        else {
            continue;
        };
        let Ok(windows) = application_proxy.get_children().await else {
            continue;
        };
        for window in windows {
            let Ok(proxy) = window.as_accessible_proxy(connection.connection()).await else {
                continue;
            };
            let Ok(states) = proxy.get_state().await else {
                continue;
            };
            if (states.contains(State::Active) || states.contains(State::Focused))
                && let Some(object) = find_focused(&connection, vec![window]).await
            {
                return finish_capture(connection, object).await;
            }
        }
    }

    // Some toolkits omit Active on their top-level window. A bounded fallback
    // search preserves compatibility while the outer deadline still fails closed.
    if let Some(object) = find_focused(&connection, applications).await {
        return finish_capture(connection, object).await;
    }
    Err("no focused AT-SPI text control was found; using overlay and clipboard".into())
}

async fn find_focused(
    connection: &AccessibilityConnection,
    roots: Vec<ObjectRefOwned>,
) -> Option<ObjectRefOwned> {
    let mut queue: VecDeque<ObjectRefOwned> = roots.into();
    let mut visited = 0_usize;
    while let Some(object) = queue.pop_front() {
        if visited >= MAX_VISITED_NODES {
            return None;
        }
        visited += 1;
        let Ok(proxy) = object.as_accessible_proxy(connection.connection()).await else {
            continue;
        };
        let Ok(states) = proxy.get_state().await else {
            continue;
        };
        if states.contains(State::Focused) {
            return Some(object);
        }
        if states.contains(State::Defunct) || states.contains(State::Stale) {
            continue;
        }
        if let Ok(children) = proxy.get_children().await {
            queue.extend(children.into_iter().filter(|child| !child.is_null()));
        }
    }
    None
}

async fn finish_capture(
    connection: AccessibilityConnection,
    object: ObjectRefOwned,
) -> Result<PlatformTarget, String> {
    validate_accessible(&connection, &object).await?;
    let snapshot = snapshot(&connection, &object).await?;
    Ok(PlatformTarget {
        connection,
        object,
        tracked: TrackedText::new(
            snapshot.value,
            snapshot.selection,
            OffsetUnit::UnicodeScalar,
        )?,
    })
}

async fn validate_accessible(
    connection: &AccessibilityConnection,
    object: &ObjectRefOwned,
) -> Result<(), String> {
    let accessible = object
        .as_accessible_proxy(connection.connection())
        .await
        .map_err(|error| format!("the focused AT-SPI object disappeared: {error}"))?;
    let states = accessible
        .get_state()
        .await
        .map_err(|error| format!("cannot read focused target states: {error}"))?;
    for required in [
        State::Focused,
        State::Editable,
        State::Enabled,
        State::Sensitive,
    ] {
        if !states.contains(required) {
            return Err(format!(
                "the focused control is not {}; using overlay and clipboard",
                required.to_static_str()
            ));
        }
    }
    if [State::Defunct, State::Stale, State::ReadOnly]
        .into_iter()
        .any(|state| states.contains(state))
    {
        return Err("the focused control is defunct, stale, or read-only".into());
    }
    let role = accessible
        .get_role()
        .await
        .map_err(|error| format!("cannot read the focused target role: {error}"))?;
    if matches!(role, Role::PasswordText | Role::Terminal) {
        return Err("password and terminal controls are never direct-insertion targets".into());
    }
    if !matches!(
        role,
        Role::Entry | Role::Text | Role::Paragraph | Role::Editbar | Role::DocumentText
    ) {
        return Err(format!(
            "the focused AT-SPI role ({role:?}) is not an approved editable-text role"
        ));
    }
    let interfaces = accessible
        .get_interfaces()
        .await
        .map_err(|error| format!("cannot inspect the focused target interfaces: {error}"))?;
    if !interfaces.contains(Interface::Text) || !interfaces.contains(Interface::EditableText) {
        return Err("the focused control does not implement Text and EditableText".into());
    }
    let attributes = accessible.get_attributes().await.map_err(|error| {
        format!(
            "cannot verify whether the focused control is protected: {error}; using overlay and clipboard"
        )
    })?;
    let protected = attributes.iter().any(|(key, value)| {
        let description = format!("{key}={value}").to_ascii_lowercase();
        ["password", "protected", "secret"]
            .iter()
            .any(|marker| description.contains(marker))
    });
    if protected {
        return Err("the focused control reports protected or secret content".into());
    }
    Ok(())
}

struct Snapshot {
    value: String,
    selection: TextRange,
}

async fn snapshot(
    connection: &AccessibilityConnection,
    object: &ObjectRefOwned,
) -> Result<Snapshot, String> {
    let text = text_proxy(connection.connection(), object).await?;
    let count = text
        .character_count()
        .await
        .map_err(|error| format!("cannot read target character count: {error}"))?;
    if !(0..=MAX_FIELD_CHARACTERS).contains(&count) {
        return Err("the focused field is too large or returned an invalid length".into());
    }
    let value = text
        .get_text(0, count)
        .await
        .map_err(|error| format!("cannot read target text: {error}"))?;
    let selection_count = text
        .get_n_selections()
        .await
        .map_err(|error| format!("cannot read target selections: {error}"))?;
    let (start, end) = match selection_count {
        0 => {
            let caret = text
                .caret_offset()
                .await
                .map_err(|error| format!("cannot read target caret: {error}"))?;
            (caret, caret)
        }
        1 => text
            .get_selection(0)
            .await
            .map_err(|error| format!("cannot read target selection: {error}"))?,
        _ => return Err("multi-range text selections are not safe insertion targets".into()),
    };
    let start = usize::try_from(start).map_err(|_| "target selection starts before zero")?;
    let end = usize::try_from(end).map_err(|_| "target selection ends before zero")?;
    Ok(Snapshot {
        value,
        selection: TextRange { start, end },
    })
}

async fn text_proxy<'a>(
    connection: &'a zbus::Connection,
    object: &ObjectRefOwned,
) -> Result<TextProxy<'a>, String> {
    let name = object
        .name()
        .ok_or_else(|| "AT-SPI returned a null target reference".to_string())?;
    TextProxy::builder(connection)
        .destination(name.clone())
        .map_err(|error| error.to_string())?
        .path(object.path().clone())
        .map_err(|error| error.to_string())?
        .cache_properties(CacheProperties::No)
        .build()
        .await
        .map_err(|error| format!("cannot create the target Text proxy: {error}"))
}

async fn editable_proxy<'a>(
    connection: &'a zbus::Connection,
    object: &ObjectRefOwned,
) -> Result<EditableTextProxy<'a>, String> {
    let name = object
        .name()
        .ok_or_else(|| "AT-SPI returned a null target reference".to_string())?;
    EditableTextProxy::builder(connection)
        .destination(name.clone())
        .map_err(|error| error.to_string())?
        .path(object.path().clone())
        .map_err(|error| error.to_string())?
        .cache_properties(CacheProperties::No)
        .build()
        .await
        .map_err(|error| format!("cannot create the target EditableText proxy: {error}"))
}

pub(super) async fn insert_text(target: &mut PlatformTarget, text: &str) -> Result<(), String> {
    tokio::time::timeout(MUTATION_TIMEOUT, async {
        revalidate(target).await?;
        let plan = target.tracked.plan_insert(text)?;
        apply(target, plan).await
    })
    .await
    .map_err(|_| "AT-SPI insertion timed out; the target lease was invalidated".to_string())?
}

pub(super) async fn replace_text(
    target: &mut PlatformTarget,
    shadow: &str,
    start_grapheme: usize,
    end_grapheme: usize,
    replacement: &str,
) -> Result<(), String> {
    tokio::time::timeout(MUTATION_TIMEOUT, async {
        revalidate(target).await?;
        let plan = target
            .tracked
            .plan_patch(shadow, start_grapheme, end_grapheme, replacement)?;
        apply(target, plan).await
    })
    .await
    .map_err(|_| "AT-SPI correction timed out; the target lease was invalidated".to_string())?
}

async fn revalidate(target: &PlatformTarget) -> Result<(), String> {
    validate_accessible(&target.connection, &target.object).await?;
    let current = snapshot(&target.connection, &target.object).await?;
    target.tracked.verify(&current.value, current.selection)
}

async fn apply(target: &mut PlatformTarget, plan: MutationPlan) -> Result<(), String> {
    let start = i32::try_from(plan.range.start).map_err(|_| "target range is too large")?;
    let end = i32::try_from(plan.range.end).map_err(|_| "target range is too large")?;
    let inserted =
        i32::try_from(plan.replacement.chars().count()).map_err(|_| "replacement is too large")?;
    let editable = editable_proxy(target.connection.connection(), &target.object).await?;
    if start != end
        && !editable.delete_text(start, end).await.map_err(|error| {
            format!("AT-SPI rejected deletion of the selected target range: {error}")
        })?
    {
        return Err("AT-SPI refused to delete the selected target range".into());
    }
    if !plan.replacement.is_empty()
        && !editable
            .insert_text(start, &plan.replacement, inserted)
            .await
            .map_err(|error| format!("AT-SPI rejected target insertion: {error}"))?
    {
        return Err("AT-SPI refused to insert text into the target".into());
    }
    let text = text_proxy(target.connection.connection(), &target.object).await?;
    let caret =
        i32::try_from(plan.next_selection.start).map_err(|_| "target range is too large")?;
    if !text
        .set_caret_offset(caret)
        .await
        .map_err(|error| format!("AT-SPI could not restore the target caret: {error}"))?
    {
        return Err("AT-SPI refused to restore the target caret".into());
    }
    let current = snapshot(&target.connection, &target.object).await?;
    if current.value != plan.next_value || current.selection != plan.next_selection {
        return Err(
            "the target did not confirm the exact value and caret after insertion; the lease is invalid"
                .into(),
        );
    }
    target.tracked.commit(plan);
    Ok(())
}
