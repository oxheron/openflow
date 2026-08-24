use openflow_desktop::hotkey::accelerator_to_xdg;

#[test]
fn converts_default_accelerator_to_xdg_shortcuts_syntax() {
    assert_eq!(
        accelerator_to_xdg("CommandOrControl+Shift+Space").unwrap(),
        "CTRL+SHIFT+space"
    );
}

#[test]
fn converts_common_named_keys_and_function_keys() {
    assert_eq!(accelerator_to_xdg("Alt+Enter").unwrap(), "ALT+Return");
    assert_eq!(accelerator_to_xdg("Ctrl+F12").unwrap(), "CTRL+F12");
    assert_eq!(
        accelerator_to_xdg("Meta+PageDown").unwrap(),
        "LOGO+Page_Down"
    );
}

#[test]
fn rejects_ambiguous_or_invalid_accelerators() {
    assert!(accelerator_to_xdg("Ctrl+Shift").is_err());
    assert!(accelerator_to_xdg("Ctrl+A+B").is_err());
    assert!(accelerator_to_xdg("Ctrl+Ctrl+A").is_err());
    assert!(accelerator_to_xdg("Ctrl++A").is_err());
}
