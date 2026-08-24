use openflow_desktop::platform::{OffsetUnit, TextRange, TrackedText};

#[test]
fn initial_selection_is_replaced_then_owned_span_grows() {
    let mut tracked = TrackedText::new(
        "before OLD after".into(),
        TextRange { start: 7, end: 10 },
        OffsetUnit::UnicodeScalar,
    )
    .unwrap();
    let first = tracked.plan_insert("new").unwrap();
    assert_eq!(first.range, TextRange { start: 7, end: 10 });
    assert_eq!(first.next_value, "before new after");
    tracked.commit(first);
    let second = tracked.plan_insert(" text").unwrap();
    assert_eq!(second.range, TextRange { start: 10, end: 10 });
    assert_eq!(second.next_value, "before new text after");
}

#[test]
fn patch_maps_graphemes_to_atspi_scalar_offsets() {
    let mut tracked = TrackedText::new(
        "x ".into(),
        TextRange { start: 2, end: 2 },
        OffsetUnit::UnicodeScalar,
    )
    .unwrap();
    let insert = tracked.plan_insert("A👨‍👩‍👧‍👦B").unwrap();
    tracked.commit(insert);
    let patch = tracked.plan_patch("A👨‍👩‍👧‍👦B", 1, 2, "🙂").unwrap();
    assert_eq!(patch.range, TextRange { start: 3, end: 10 });
    assert_eq!(patch.next_value, "x A🙂B");
}

#[test]
fn patch_maps_graphemes_to_ax_utf16_offsets() {
    let mut tracked = TrackedText::new(
        "x ".into(),
        TextRange { start: 2, end: 2 },
        OffsetUnit::Utf16,
    )
    .unwrap();
    let insert = tracked.plan_insert("A😀B").unwrap();
    tracked.commit(insert);
    let patch = tracked.plan_patch("A😀B", 1, 2, "é").unwrap();
    assert_eq!(patch.range, TextRange { start: 3, end: 5 });
    assert_eq!(patch.next_value, "x AéB");
}

#[test]
fn external_value_or_selection_change_invalidates_target() {
    let tracked = TrackedText::new(
        "hello".into(),
        TextRange { start: 5, end: 5 },
        OffsetUnit::UnicodeScalar,
    )
    .unwrap();
    assert!(
        tracked
            .verify("hello!", TextRange { start: 5, end: 5 })
            .is_err()
    );
    assert!(
        tracked
            .verify("hello", TextRange { start: 4, end: 4 })
            .is_err()
    );
}
