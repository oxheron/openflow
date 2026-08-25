use openflow_server::rolling_consensus::{
    ConsensusConfig, ConsensusError, Hypothesis, Pass, RollingConsensus, TimedWord,
};

fn word(text: &str, start_ms: u64, end_ms: u64) -> TimedWord {
    TimedWord {
        text: text.to_owned(),
        start_ms,
        end_ms,
        probability: Some(0.9),
    }
}

fn hypothesis(words: Vec<TimedWord>, score: f32) -> Hypothesis {
    Hypothesis {
        words,
        normalized_log_probability: Some(score),
    }
}

fn pass(end_ms: u64, hypotheses: Vec<Hypothesis>) -> Pass {
    Pass {
        window_start_ms: 0,
        window_end_ms: end_ms,
        hypotheses,
    }
}

fn tracker() -> RollingConsensus {
    RollingConsensus::new(ConsensusConfig::default()).expect("valid config")
}

#[test]
fn config_rejects_unbounded_or_single_pass_consensus() {
    let error = RollingConsensus::new(ConsensusConfig {
        history_size: 4,
        ..ConsensusConfig::default()
    })
    .expect_err("history must be bounded");
    assert_eq!(error, ConsensusError::InvalidHistorySize);

    let error = RollingConsensus::new(ConsensusConfig {
        agreement_passes: 1,
        ..ConsensusConfig::default()
    })
    .expect_err("one pass is not consensus");
    assert_eq!(error, ConsensusError::InvalidAgreementPasses);
}

#[test]
fn commits_only_mature_words_after_three_normalized_word_agreements() {
    let mut consensus = tracker();
    let variants = ["Hello,", "hello", "HELLO!"];
    for (index, text) in variants.into_iter().enumerate() {
        let end = 8_000 + index as u64 * 1_000;
        let update = consensus
            .observe(pass(
                end,
                vec![hypothesis(
                    vec![word(text, 500, 1_000), word("world", 4_500, 5_000)],
                    -0.2,
                )],
            ))
            .expect("valid pass");
        if index < 2 {
            assert!(update.committed_text.is_empty());
        } else {
            assert_eq!(update.committed_append, "HELLO!");
            assert_eq!(update.committed_text, "HELLO!");
            assert_eq!(update.best_unstable_text, " world");
            assert_eq!(update.best_text(), "HELLO! world");
        }
    }
}

#[test]
fn disagreement_reports_supported_candidates_and_blocks_later_words() {
    let mut consensus = tracker();
    let alternatives = ["their", "there", "their"];
    for (index, ambiguous) in alternatives.into_iter().enumerate() {
        let update = consensus
            .observe(pass(
                10_000 + index as u64 * 1_000,
                vec![hypothesis(
                    vec![
                        word("open", 500, 900),
                        word(ambiguous, 1_000, 1_400),
                        word("project", 1_500, 2_000),
                    ],
                    -0.1,
                )],
            ))
            .expect("valid pass");
        if index == 2 {
            assert_eq!(update.committed_text, "open");
            assert_eq!(update.ambiguities.len(), 1);
            let ambiguity = &update.ambiguities[0];
            assert_eq!(ambiguity.start_ms, 1_000);
            assert!(ambiguity.candidates.iter().any(|candidate| {
                candidate.text == "their project" && candidate.pass_support == 2
            }));
            assert!(ambiguity.candidates.iter().any(|candidate| {
                candidate.text == "there project" && candidate.pass_support == 1
            }));
            // "project" agrees later, but cannot leap over the unresolved word.
            assert_eq!(update.committed_text, "open");
        }
    }
}

#[test]
fn new_consensus_eventually_commits_after_old_disagreement_ages_out() {
    let mut consensus = tracker();
    for (index, text) in ["cat", "cap", "cat", "cat", "cat"].into_iter().enumerate() {
        let update = consensus
            .observe(pass(
                10_000 + index as u64 * 1_000,
                vec![hypothesis(vec![word(text, 1_000, 1_500)], -0.2)],
            ))
            .expect("valid pass");
        if index == 4 {
            assert_eq!(update.committed_text, "cat");
        }
    }
    assert_eq!(consensus.history_len(), 3);
}

#[test]
fn shared_n_best_path_can_outvote_disagreeing_top_beams() {
    let mut consensus = tracker();
    for (index, top) in ["write", "right", "rite"].into_iter().enumerate() {
        let update = consensus
            .observe(pass(
                10_000 + index as u64 * 1_000,
                vec![
                    hypothesis(vec![word(top, 500, 900)], -0.1),
                    hypothesis(vec![word("write", 500, 900)], -0.3),
                ],
            ))
            .expect("valid pass");
        if index == 2 {
            assert_eq!(update.committed_text, "write");
            assert!(update.ambiguities.is_empty());
        }
    }
}

#[test]
fn committed_overlap_is_aligned_and_never_duplicated_or_revised() {
    let mut consensus = tracker();
    for index in 0..3 {
        consensus
            .observe(pass(
                10_000 + index * 1_000,
                vec![hypothesis(vec![word("hello", 500, 1_000)], -0.1)],
            ))
            .expect("valid pass");
    }
    assert_eq!(consensus.committed_text(), "hello");

    for index in 0..3 {
        let update = consensus
            .observe(pass(
                13_000 + index * 1_000,
                vec![hypothesis(
                    vec![
                        // Timestamp drift means time filtering alone would
                        // treat this as a new word; alignment removes it.
                        word("HELLO", 650, 1_150),
                        word("world", 1_300, 1_800),
                    ],
                    -0.1,
                )],
            ))
            .expect("valid pass");
        assert!(!update.best_text().starts_with("hello HELLO"));
    }
    assert_eq!(consensus.committed_text(), "hello world");

    for index in 0..3 {
        consensus
            .observe(pass(
                16_000 + index * 1_000,
                vec![hypothesis(
                    vec![word("yellow", 500, 1_000), word("again", 2_000, 2_500)],
                    -0.1,
                )],
            ))
            .expect("valid pass");
    }
    assert_eq!(consensus.committed_text(), "hello world again");
}

#[test]
fn finalize_uses_cross_pass_support_then_clears_history() {
    let mut consensus = tracker();
    consensus
        .observe(pass(
            2_000,
            vec![
                hypothesis(vec![word("write", 500, 900)], -0.4),
                hypothesis(vec![word("right", 500, 900)], -0.1),
            ],
        ))
        .expect("valid pass");
    consensus
        .observe(pass(
            4_000,
            vec![
                hypothesis(vec![word("write", 500, 900)], -0.4),
                hypothesis(vec![word("right", 500, 900)], -0.1),
            ],
        ))
        .expect("valid pass");

    let update = consensus.finalize();
    assert_eq!(update.committed_append, "right");
    assert_eq!(update.best_text(), "right");
    assert_eq!(consensus.history_len(), 0);
}

#[test]
fn ambiguity_resolution_requires_recent_acoustic_support() {
    let mut consensus = tracker();
    for (index, ambiguous) in ["pie torch", "pie torch", "PyTorch"]
        .into_iter()
        .enumerate()
    {
        let words = ambiguous
            .split_whitespace()
            .enumerate()
            .map(|(word_index, text)| {
                word(
                    text,
                    500 + word_index as u64 * 500,
                    900 + word_index as u64 * 500,
                )
            })
            .collect();
        consensus
            .observe(pass(
                10_000 + index as u64 * 1_000,
                vec![hypothesis(words, -0.1)],
            ))
            .expect("valid pass");
    }

    assert_eq!(
        consensus.resolve_ambiguity("invented wording", "invented wording"),
        Err(ConsensusError::UnsupportedResolution)
    );
    let update = consensus
        .resolve_ambiguity("pie torch", "PyTorch")
        .expect("recent ASR-supported wording");
    assert_eq!(update.committed_text, "PyTorch");
}

#[test]
fn rejects_relative_or_non_monotonic_timestamps() {
    let mut consensus = tracker();
    let error = consensus
        .observe(Pass {
            window_start_ms: 20_000,
            window_end_ms: 45_000,
            hypotheses: vec![hypothesis(vec![word("relative", 0, 500)], -0.1)],
        })
        .expect_err("timestamps must be absolute");
    assert_eq!(
        error,
        ConsensusError::InvalidWordTimestamps { hypothesis: 0 }
    );

    consensus
        .observe(pass(1_000, Vec::new()))
        .expect("empty speech pass is valid");
    assert_eq!(
        consensus.observe(pass(1_000, Vec::new())),
        Err(ConsensusError::NonIncreasingPass)
    );
}
