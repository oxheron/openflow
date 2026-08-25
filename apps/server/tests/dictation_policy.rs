use openflow_server::{
    NormalizationProposal,
    rolling_consensus::{AmbiguousSpan, CandidateEvidence, ConsensusUpdate},
    routes::{
        apply_normalizations, language_left_context, language_right_context, rolling_audio_window,
    },
};

fn proposal(
    start_byte: usize,
    end_byte: usize,
    source: &str,
    replacement: &str,
    kind: &str,
    grounding: &str,
) -> NormalizationProposal {
    NormalizationProposal {
        start_byte,
        end_byte,
        source: source.into(),
        replacement: replacement.into(),
        kind: kind.into(),
        grounding: grounding.into(),
    }
}

#[test]
fn rolling_window_retains_the_latest_complete_pcm_samples() {
    let audio = (0_u8..20).collect::<Vec<_>>();
    let (window, start_ms, end_ms) = rolling_audio_window(&audio, 8);
    assert_eq!(window, &[12, 13, 14, 15, 16, 17, 18, 19]);
    assert_eq!(start_ms, 0);
    assert_eq!(end_ms, 0);

    let twenty_six_seconds = vec![0; 26 * 16_000 * 2 + 1];
    let (window, start_ms, end_ms) = rolling_audio_window(&twenty_six_seconds, 25 * 16_000 * 2);
    assert_eq!(window.len(), 25 * 16_000 * 2);
    assert_eq!(start_ms, 1_000);
    assert_eq!(end_ms, 26_000);
}

#[test]
fn normalization_is_exact_local_and_utf8_safe() {
    let text = "use pie torch and see plus plus";
    let proposals = vec![
        proposal(
            4,
            13,
            "pie torch",
            "PyTorch",
            "canonical_name",
            "phonetic_equivalence",
        ),
        proposal(
            18,
            31,
            "see plus plus",
            "C++",
            "spoken_symbol",
            "spoken_symbol",
        ),
    ];
    assert_eq!(
        apply_normalizations(text, &proposals, &[]).as_deref(),
        Some("use PyTorch and C++")
    );

    let mut wrong_source = proposals.clone();
    wrong_source[0].source = "invented".into();
    assert!(apply_normalizations(text, &wrong_source, &[]).is_none());

    let unicode = "café";
    assert!(
        apply_normalizations(
            unicode,
            &[proposal(
                0,
                4,
                "caf",
                "Cafe",
                "formatting",
                "lexical_skeleton",
            )],
            &[],
        )
        .is_none()
    );
}

#[test]
fn lexical_normalization_requires_a_known_or_glossary_alias() {
    let unsupported = proposal(0, 3, "cat", "Car", "canonical_name", "phonetic_equivalence");
    assert!(apply_normalizations("cat", &[unsupported], &[]).is_none());

    let glossary = vec!["Kubernetes".to_owned()];
    let supported = proposal(
        0,
        11,
        "Kubernetees",
        "Kubernetes",
        "canonical_name",
        "phonetic_equivalence",
    );
    assert_eq!(
        apply_normalizations("Kubernetees", &[supported], &glossary).as_deref(),
        Some("Kubernetes")
    );

    let spoken_letters = proposal(
        0,
        7,
        "jay son",
        "JSON",
        "canonical_name",
        "phonetic_equivalence",
    );
    assert_eq!(
        apply_normalizations("jay son", &[spoken_letters], &["JSON".into()]).as_deref(),
        Some("JSON")
    );
}

#[test]
fn language_context_preserves_candidate_boundaries() {
    let context = language_left_context("a long committed prefix", 16);
    assert_eq!(context, "committed prefix ");

    let update = ConsensusUpdate {
        best_unstable_text: " pie torch is useful".into(),
        ..ConsensusUpdate::default()
    };
    let ambiguity = AmbiguousSpan {
        start_ms: 0,
        end_ms: 1,
        candidates: vec![CandidateEvidence {
            text: "pie torch".into(),
            pass_support: 2,
            hypothesis_support: 2,
            best_rank: 0,
            best_normalized_log_probability: None,
            best_mean_word_probability: None,
        }],
    };
    assert_eq!(
        language_right_context(&update, &ambiguity, 1_024),
        " is useful"
    );
}
