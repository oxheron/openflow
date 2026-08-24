use openflow_desktop::target::{TargetError, TargetState, replace_graphemes};

#[test]
fn replaces_unicode_graphemes_without_splitting_them() {
    assert_eq!(replace_graphemes("Hi 👨‍👩‍👧‍👦!", 3, 4, "🙂"), Ok("Hi 🙂!".into()));
}

#[test]
fn rejects_an_out_of_bounds_patch() {
    assert_eq!(
        replace_graphemes("short", 2, 9, "x"),
        Err(TargetError::InvalidRange)
    );
}

#[tokio::test]
async fn stale_release_cannot_drop_a_newer_lease() {
    let targets = TargetState::default();
    let first = targets.capture().await.expect("first lease");
    let second = targets.capture().await.expect("second lease");
    assert_eq!(
        targets.release(first.lease_id()).await,
        Err(TargetError::StaleLease)
    );
    assert!(targets.release(second.lease_id()).await.is_ok());
}
