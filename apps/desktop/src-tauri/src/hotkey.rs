use std::collections::HashSet;

/// Converts a Tauri accelerator into the freedesktop shortcuts syntax.
///
/// The XDG Global Shortcuts portal uses xkbcommon modifier and keysym names,
/// for example `CTRL+SHIFT+space`.
#[doc(hidden)]
pub fn accelerator_to_xdg(accelerator: &str) -> Result<String, String> {
    let mut modifiers = Vec::new();
    let mut seen_modifiers = HashSet::new();
    let mut key = None;

    for raw_part in accelerator.split('+') {
        let part = raw_part.trim();
        if part.is_empty() {
            return Err("hotkey contains an empty key or modifier".into());
        }

        let modifier = match part.to_ascii_lowercase().as_str() {
            "commandorcontrol" | "commandorctrl" | "control" | "ctrl" => Some("CTRL"),
            "alt" | "option" => Some("ALT"),
            "shift" => Some("SHIFT"),
            "super" | "meta" | "command" | "cmd" => Some("LOGO"),
            "num" | "numlock" => Some("NUM"),
            _ => None,
        };
        if let Some(modifier) = modifier {
            if !seen_modifiers.insert(modifier) {
                return Err(format!(
                    "hotkey contains the {modifier} modifier more than once"
                ));
            }
            modifiers.push(modifier);
            continue;
        }

        if key.is_some() {
            return Err("hotkey must contain exactly one non-modifier key".into());
        }
        key = Some(xdg_keysym(part)?);
    }

    let key = key.ok_or_else(|| "hotkey must contain a non-modifier key".to_owned())?;
    modifiers.push(&key);
    Ok(modifiers.join("+"))
}

fn xdg_keysym(key: &str) -> Result<String, String> {
    let normalized = match key.to_ascii_lowercase().as_str() {
        "space" => "space".to_owned(),
        "enter" | "return" => "Return".to_owned(),
        "esc" | "escape" => "Escape".to_owned(),
        "tab" => "Tab".to_owned(),
        "backspace" => "BackSpace".to_owned(),
        "delete" | "del" => "Delete".to_owned(),
        "insert" | "ins" => "Insert".to_owned(),
        "home" => "Home".to_owned(),
        "end" => "End".to_owned(),
        "pageup" | "page_up" => "Page_Up".to_owned(),
        "pagedown" | "page_down" => "Page_Down".to_owned(),
        "left" | "arrowleft" => "Left".to_owned(),
        "right" | "arrowright" => "Right".to_owned(),
        "up" | "arrowup" => "Up".to_owned(),
        "down" | "arrowdown" => "Down".to_owned(),
        _ if key.len() == 1 && key.as_bytes()[0].is_ascii_alphanumeric() => {
            key.to_ascii_lowercase()
        }
        _ if is_function_key(key) => key.to_ascii_uppercase(),
        _ if key
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '_') =>
        {
            key.to_owned()
        }
        _ => {
            return Err(format!(
                "hotkey key `{key}` cannot be represented by the XDG shortcuts specification"
            ));
        }
    };
    Ok(normalized)
}

fn is_function_key(key: &str) -> bool {
    let Some(number) = key
        .strip_prefix('F')
        .or_else(|| key.strip_prefix('f'))
        .and_then(|value| value.parse::<u8>().ok())
    else {
        return false;
    };
    (1..=35).contains(&number)
}

#[cfg(target_os = "linux")]
mod linux {
    use std::{collections::HashMap, time::Duration};

    use futures_util::StreamExt;
    use tauri::{AppHandle, Emitter};
    use tokio::{sync::oneshot, task::JoinHandle, time::timeout};
    use uuid::Uuid;
    use zbus::{
        Connection, Proxy,
        proxy::SignalStream,
        zvariant::{OwnedObjectPath, OwnedValue, Str},
    };

    use super::accelerator_to_xdg;

    const PORTAL_DESTINATION: &str = "org.freedesktop.portal.Desktop";
    const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";
    const SHORTCUTS_INTERFACE: &str = "org.freedesktop.portal.GlobalShortcuts";
    const REQUEST_INTERFACE: &str = "org.freedesktop.portal.Request";
    const SESSION_INTERFACE: &str = "org.freedesktop.portal.Session";
    const SHORTCUT_ID: &str = "toggle-dictation";
    const CREATE_TIMEOUT: Duration = Duration::from_secs(30);
    const PERMISSION_TIMEOUT: Duration = Duration::from_secs(300);

    type VariantMap = HashMap<String, OwnedValue>;
    type Shortcut = (String, VariantMap);

    #[derive(Debug, Default)]
    struct StateInner {
        generation: u64,
        pending: Option<PendingSetup>,
        registration: Option<ActiveRegistration>,
    }

    #[derive(Debug, Default)]
    pub struct WaylandHotkeyState {
        inner: tokio::sync::Mutex<StateInner>,
    }

    #[derive(Debug)]
    struct PendingSetup {
        id: String,
        cancel: oneshot::Sender<()>,
    }

    #[derive(Debug)]
    struct ActiveRegistration {
        id: String,
        portal: PortalRegistration,
    }

    #[derive(Debug)]
    struct PortalRegistration {
        cancel: Option<oneshot::Sender<()>>,
        task: JoinHandle<()>,
    }

    impl PortalRegistration {
        async fn close(mut self) {
            if let Some(cancel) = self.cancel.take() {
                let _ = cancel.send(());
            }
            let _ = timeout(Duration::from_secs(3), self.task).await;
        }
    }

    struct PortalSetup {
        connection: Connection,
        session: OwnedObjectPath,
        activations: SignalStream<'static>,
    }

    impl PortalSetup {
        fn start(self, app: AppHandle) -> PortalRegistration {
            let (cancel, cancelled) = oneshot::channel();
            let task = tokio::spawn(run_portal_session(self, app, cancelled));
            PortalRegistration {
                cancel: Some(cancel),
                task,
            }
        }

        async fn close(self) {
            close_session(&self.connection, &self.session).await;
        }
    }

    pub async fn register(
        app: AppHandle,
        state: &WaylandHotkeyState,
        accelerator: &str,
        registration_id: &str,
    ) -> Result<bool, String> {
        if !is_wayland_session() {
            return Ok(false);
        }
        validate_registration_id(registration_id)?;

        let preferred_trigger = accelerator_to_xdg(accelerator)?;
        let (setup_cancel, mut setup_cancelled) = oneshot::channel();
        let (generation, previous_setup, previous_registration) = {
            let mut inner = state.inner.lock().await;
            inner.generation = inner.generation.wrapping_add(1);
            let previous_setup = inner.pending.replace(PendingSetup {
                id: registration_id.to_owned(),
                cancel: setup_cancel,
            });
            (inner.generation, previous_setup, inner.registration.take())
        };
        if let Some(previous_setup) = previous_setup {
            let _ = previous_setup.cancel.send(());
        }
        if let Some(previous) = previous_registration {
            previous.portal.close().await;
        }

        let setup_result = tokio::select! {
            result = setup_portal(&preferred_trigger) => result,
            _ = &mut setup_cancelled => {
                Err("Wayland global-shortcut registration was cancelled".to_owned())
            }
        };
        let mut inner = state.inner.lock().await;
        let is_current = inner.generation == generation
            && inner
                .pending
                .as_ref()
                .is_some_and(|pending| pending.id == registration_id);
        if is_current {
            inner.pending = None;
            let setup = setup_result?;
            inner.registration = Some(ActiveRegistration {
                id: registration_id.to_owned(),
                portal: setup.start(app),
            });
            Ok(true)
        } else {
            drop(inner);
            if let Ok(setup) = setup_result {
                setup.close().await;
            }
            Err("Wayland global-shortcut registration was superseded".into())
        }
    }

    pub async fn unregister(state: &WaylandHotkeyState, registration_id: &str) {
        let (pending, registration) = {
            let mut inner = state.inner.lock().await;
            let matches_pending = inner
                .pending
                .as_ref()
                .is_some_and(|pending| pending.id == registration_id);
            let matches_registration = inner
                .registration
                .as_ref()
                .is_some_and(|registration| registration.id == registration_id);
            if matches_pending || matches_registration {
                inner.generation = inner.generation.wrapping_add(1);
            }
            let pending = if matches_pending {
                inner.pending.take()
            } else {
                None
            };
            let registration = if matches_registration {
                inner.registration.take()
            } else {
                None
            };
            (pending, registration)
        };
        if let Some(pending) = pending {
            let _ = pending.cancel.send(());
        }
        if let Some(registration) = registration {
            registration.portal.close().await;
        }
    }

    fn validate_registration_id(registration_id: &str) -> Result<(), String> {
        if registration_id.is_empty()
            || registration_id.len() > 128
            || !registration_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        {
            return Err("hotkey registration ID is invalid".into());
        }
        Ok(())
    }

    fn is_wayland_session() -> bool {
        match std::env::var("XDG_SESSION_TYPE") {
            Ok(session_type) => session_type.eq_ignore_ascii_case("wayland"),
            Err(_) => std::env::var_os("WAYLAND_DISPLAY").is_some_and(|value| !value.is_empty()),
        }
    }

    async fn setup_portal(preferred_trigger: &str) -> Result<PortalSetup, String> {
        let connection = Connection::session().await.map_err(|error| {
            format!("cannot connect to the desktop portal session bus: {error}")
        })?;
        let portal = Proxy::new_owned(
            connection.clone(),
            PORTAL_DESTINATION,
            PORTAL_PATH,
            SHORTCUTS_INTERFACE,
        )
        .await
        .map_err(portal_unavailable)?;
        let _: u32 = portal
            .get_property("version")
            .await
            .map_err(portal_unavailable)?;

        let create_token = request_token("create");
        let (create_path, create_responses) = response_listener(&connection, &create_token).await?;
        let mut create_options = VariantMap::new();
        create_options.insert("handle_token".into(), string_value(create_token));
        create_options.insert(
            "session_handle_token".into(),
            string_value(request_token("session")),
        );
        let returned_path: OwnedObjectPath = portal
            .call_method("CreateSession", &create_options)
            .await
            .map_err(portal_unavailable)?
            .body()
            .deserialize()
            .map_err(|error| format!("portal returned an invalid CreateSession handle: {error}"))?;
        verify_request_path(&create_path, &returned_path)?;
        let mut create_results = await_response(
            create_responses,
            "create the shortcut session",
            CREATE_TIMEOUT,
        )
        .await?;
        let session_string = create_results
            .remove("session_handle")
            .ok_or_else(|| "portal did not return a global-shortcut session handle".to_owned())
            .and_then(|value| {
                String::try_from(value)
                    .map_err(|error| format!("portal returned an invalid session handle: {error}"))
            })?;
        let session = OwnedObjectPath::try_from(session_string)
            .map_err(|error| format!("portal returned an invalid session object path: {error}"))?;

        let activations = portal
            .receive_signal_with_args("Activated", &[(0, session.as_str())])
            .await
            .map_err(portal_unavailable)?;

        let bind_result = bind_shortcut(&connection, &portal, &session, preferred_trigger).await;
        if let Err(error) = bind_result {
            close_session(&connection, &session).await;
            return Err(error);
        }

        // Keep the listener alive from before BindShortcuts completes; this avoids
        // missing a very early activation after the compositor grants permission.
        Ok(PortalSetup {
            connection,
            session,
            activations,
        })
    }

    async fn bind_shortcut(
        connection: &Connection,
        portal: &Proxy<'_>,
        session: &OwnedObjectPath,
        preferred_trigger: &str,
    ) -> Result<(), String> {
        let bind_token = request_token("bind");
        let (bind_path, bind_responses) = response_listener(connection, &bind_token).await?;
        let mut properties = VariantMap::new();
        properties.insert(
            "description".into(),
            string_value("Start or stop OpenFlow dictation".to_owned()),
        );
        properties.insert(
            "preferred_trigger".into(),
            string_value(preferred_trigger.to_owned()),
        );
        let shortcuts: Vec<Shortcut> = vec![(SHORTCUT_ID.to_owned(), properties)];
        let mut options = VariantMap::new();
        options.insert("handle_token".into(), string_value(bind_token));
        let returned_path: OwnedObjectPath = portal
            .call_method("BindShortcuts", &(session, shortcuts, "", options))
            .await
            .map_err(portal_unavailable)?
            .body()
            .deserialize()
            .map_err(|error| format!("portal returned an invalid BindShortcuts handle: {error}"))?;
        verify_request_path(&bind_path, &returned_path)?;
        let mut results = await_response(
            bind_responses,
            "grant global-shortcut permission",
            PERMISSION_TIMEOUT,
        )
        .await?;
        let shortcuts = results
            .remove("shortcuts")
            .ok_or_else(|| "the desktop portal did not bind the requested shortcut".to_owned())
            .and_then(|value| {
                Vec::<Shortcut>::try_from(value)
                    .map_err(|error| format!("portal returned an invalid shortcut list: {error}"))
            })?;
        if shortcuts.iter().any(|(id, _)| id == SHORTCUT_ID) {
            Ok(())
        } else {
            Err(
                "Global shortcut permission was not granted. OpenFlow can still be controlled from its tray menu."
                    .into(),
            )
        }
    }

    async fn response_listener(
        connection: &Connection,
        token: &str,
    ) -> Result<(OwnedObjectPath, SignalStream<'static>), String> {
        let sender = connection
            .unique_name()
            .ok_or_else(|| "the session bus did not assign OpenFlow a unique name".to_owned())?
            .as_str()
            .trim_start_matches(':')
            .replace('.', "_");
        let path = OwnedObjectPath::try_from(format!(
            "/org/freedesktop/portal/desktop/request/{sender}/{token}"
        ))
        .map_err(|error| format!("cannot construct a portal request path: {error}"))?;
        let request = Proxy::new_owned(
            connection.clone(),
            PORTAL_DESTINATION,
            path.clone(),
            REQUEST_INTERFACE,
        )
        .await
        .map_err(portal_unavailable)?;
        let responses = request
            .receive_signal("Response")
            .await
            .map_err(portal_unavailable)?;
        Ok((path, responses))
    }

    async fn await_response(
        mut responses: SignalStream<'static>,
        operation: &str,
        wait: Duration,
    ) -> Result<VariantMap, String> {
        let message = timeout(wait, responses.next())
            .await
            .map_err(|_| format!("timed out waiting for the desktop portal to {operation}"))?
            .ok_or_else(|| format!("desktop portal closed while trying to {operation}"))?;
        let (response, results): (u32, VariantMap) = message
            .body()
            .deserialize()
            .map_err(|error| format!("portal returned an invalid permission response: {error}"))?;
        match response {
            0 => Ok(results),
            1 => Err(format!(
                "Global shortcut permission was cancelled while trying to {operation}. OpenFlow can still be controlled from its tray menu."
            )),
            _ => Err(format!(
                "Global shortcut permission was denied or unavailable while trying to {operation}. OpenFlow can still be controlled from its tray menu."
            )),
        }
    }

    async fn run_portal_session(
        setup: PortalSetup,
        app: AppHandle,
        mut cancelled: oneshot::Receiver<()>,
    ) {
        let PortalSetup {
            connection,
            session,
            mut activations,
        } = setup;
        loop {
            tokio::select! {
                _ = &mut cancelled => break,
                message = activations.next() => {
                    let Some(message) = message else { break };
                    let body: Result<(OwnedObjectPath, String, u64, VariantMap), _> =
                        message.body().deserialize();
                    if let Ok((activated_session, shortcut_id, _, _)) = body
                        && activated_session == session
                        && shortcut_id == SHORTCUT_ID
                    {
                        let _ = app.emit("openflow://toggle-requested", ());
                    }
                }
            }
        }
        close_session(&connection, &session).await;
    }

    async fn close_session(connection: &Connection, session: &OwnedObjectPath) {
        let Ok(proxy) = Proxy::new_owned(
            connection.clone(),
            PORTAL_DESTINATION,
            session.clone(),
            SESSION_INTERFACE,
        )
        .await
        else {
            return;
        };
        let _ = timeout(Duration::from_secs(2), proxy.call_method("Close", &())).await;
    }

    fn request_token(purpose: &str) -> String {
        format!("openflow_{purpose}_{}", Uuid::new_v4().simple())
    }

    fn string_value(value: String) -> OwnedValue {
        OwnedValue::from(Str::from(value))
    }

    fn verify_request_path(
        expected: &OwnedObjectPath,
        returned: &OwnedObjectPath,
    ) -> Result<(), String> {
        if expected == returned {
            Ok(())
        } else {
            Err(format!(
                "desktop portal returned an unexpected request handle ({returned})"
            ))
        }
    }

    #[allow(clippy::needless_pass_by_value)]
    fn portal_unavailable(error: zbus::Error) -> String {
        format!(
            "The XDG Global Shortcuts portal is unavailable: {error}. Install or enable a portal backend for your Wayland compositor, or use the OpenFlow tray menu."
        )
    }
}

#[cfg(target_os = "linux")]
pub use linux::WaylandHotkeyState;

#[cfg(not(target_os = "linux"))]
#[derive(Debug, Default)]
pub struct WaylandHotkeyState;

#[cfg(target_os = "linux")]
#[tauri::command]
pub(crate) async fn register_wayland_hotkey(
    app: tauri::AppHandle,
    state: tauri::State<'_, WaylandHotkeyState>,
    accelerator: String,
    registration_id: String,
) -> Result<bool, String> {
    linux::register(app, &state, &accelerator, &registration_id).await
}

#[cfg(not(target_os = "linux"))]
#[tauri::command]
pub(crate) async fn register_wayland_hotkey(
    _app: tauri::AppHandle,
    _state: tauri::State<'_, WaylandHotkeyState>,
    _accelerator: String,
    _registration_id: String,
) -> Result<bool, String> {
    Ok(false)
}

#[cfg(target_os = "linux")]
#[tauri::command]
pub(crate) async fn unregister_wayland_hotkey(
    state: tauri::State<'_, WaylandHotkeyState>,
    registration_id: String,
) -> Result<(), String> {
    linux::unregister(&state, &registration_id).await;
    Ok(())
}

#[cfg(not(target_os = "linux"))]
#[tauri::command]
pub(crate) async fn unregister_wayland_hotkey(
    _state: tauri::State<'_, WaylandHotkeyState>,
    _registration_id: String,
) -> Result<(), String> {
    Ok(())
}
