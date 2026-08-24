use openflow_server::SessionGate;

#[test]
fn only_one_session_can_hold_the_gate() {
    let gate = SessionGate::default();
    let lease = gate.try_acquire().unwrap();
    assert!(gate.is_active());
    assert!(gate.try_acquire().is_none());
    drop(lease);
    assert!(gate.try_acquire().is_some());
}

#[cfg(target_os = "linux")]
#[test]
fn parses_largest_single_nvidia_card_memory() {
    use openflow_server::state::parse_nvidia_vram_mib;

    assert_eq!(
        parse_nvidia_vram_mib("10240\n 24576 \n"),
        Some(24_576 * 1024 * 1024)
    );
    assert_eq!(parse_nvidia_vram_mib("N/A\n"), None);
}
