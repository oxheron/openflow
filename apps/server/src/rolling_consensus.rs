//! Deterministic consensus for overlapping, timestamped ASR passes.
//!
//! [`RollingConsensus`] keeps only two or three recent recognition passes. It
//! commits a left-to-right prefix after the same lexical words occur in a
//! configurable number of consecutive passes and the words are older than the
//! unstable tail. Once appended, committed text is never revised.
//!
//! This module deliberately does not choose between lexical alternatives. It
//! reports a coalesced mature ambiguity, including ASR/pass support, for a
//! later constrained language-scoring layer. Punctuation and capitalization
//! do not prevent otherwise-identical words from agreeing, but the newest
//! supported surface form is retained.
//!
//! # Limitations
//!
//! - Inputs must already be word-like units with absolute timestamps. A
//!   punctuation mark should normally be attached to its word; standalone
//!   punctuation does not establish lexical agreement.
//! - Alignment is an exact normalized-word LCS with a timestamp tolerance. It
//!   is intentionally not phonetic, so spelling, word-boundary, and name
//!   normalization belongs in the later constrained LLM stage.
//! - Because commitment is strictly left to right, the first disagreement
//!   blocks later words. Mature unresolved words are consequently reported as
//!   one coalesced span rather than linguistically segmented clauses.
//! - The caller owns audio windowing, VAD, and conversion of model-relative
//!   timestamps to the absolute session timeline.

use std::{cmp::Ordering, collections::VecDeque};

use thiserror::Error;

const MIN_HISTORY_SIZE: usize = 2;
const MAX_HISTORY_SIZE: usize = 3;
const COMMITTED_ALIGNMENT_WORDS: usize = 32;

/// Tuning values for rolling transcript stabilization.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ConsensusConfig {
    /// Number of recent passes retained. Must be in `2..=3`.
    pub history_size: usize,
    /// Number of most recent consecutive passes that must agree. Must be in
    /// `2..=history_size`.
    pub agreement_passes: usize,
    /// Audio nearest the live edge that remains mutable.
    pub unstable_tail_ms: u64,
    /// Maximum midpoint drift for two equal words to align.
    pub alignment_tolerance_ms: u64,
}

impl Default for ConsensusConfig {
    fn default() -> Self {
        Self {
            history_size: 3,
            agreement_passes: 3,
            unstable_tail_ms: 6_000,
            alignment_tolerance_ms: 1_500,
        }
    }
}

/// A timestamped word-like ASR unit on the absolute session timeline.
#[derive(Clone, Debug, PartialEq)]
pub struct TimedWord {
    pub text: String,
    pub start_ms: u64,
    pub end_ms: u64,
    /// Optional ASR probability in the inclusive range `0..=1`.
    pub probability: Option<f32>,
}

/// One complete word sequence returned by ASR for a pass.
#[derive(Clone, Debug, PartialEq)]
pub struct Hypothesis {
    pub words: Vec<TimedWord>,
    /// Optional length-normalized ASR log probability. Larger is better.
    pub normalized_log_probability: Option<f32>,
}

/// All ASR alternatives for one rolling audio window.
#[derive(Clone, Debug, PartialEq)]
pub struct Pass {
    pub window_start_ms: u64,
    pub window_end_ms: u64,
    /// Alternatives are expected in ASR rank order when scores tie.
    pub hypotheses: Vec<Hypothesis>,
}

/// Evidence for one acoustically supported wording of an ambiguous span.
#[derive(Clone, Debug, PartialEq)]
pub struct CandidateEvidence {
    pub text: String,
    /// Number of distinct recent passes containing this wording in any beam.
    pub pass_support: usize,
    /// Number of hypotheses containing this wording.
    pub hypothesis_support: usize,
    /// Best rank (zero based) at which this wording appeared.
    pub best_rank: usize,
    pub best_normalized_log_probability: Option<f32>,
    pub best_mean_word_probability: Option<f32>,
}

/// A mature span whose selected Whisper hypotheses have not converged.
#[derive(Clone, Debug, PartialEq)]
pub struct AmbiguousSpan {
    pub start_ms: u64,
    pub end_ms: u64,
    pub candidates: Vec<CandidateEvidence>,
}

/// The incremental result of observing or finalizing ASR state.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ConsensusUpdate {
    /// Exact fragment to append to the committed text that preceded this call.
    /// It may begin with whitespace.
    pub committed_append: String,
    /// Complete immutable transcript prefix after this call.
    pub committed_text: String,
    /// Best mutable suffix, formatted for direct append to `committed_text`. It
    /// may begin with whitespace.
    pub best_unstable_text: String,
    /// Old-enough disagreements eligible for later LLM arbitration.
    pub ambiguities: Vec<AmbiguousSpan>,
}

impl ConsensusUpdate {
    /// Returns the best current display transcript.
    #[must_use]
    pub fn best_text(&self) -> String {
        let mut text = self.committed_text.clone();
        text.push_str(&self.best_unstable_text);
        text
    }
}

/// Invalid configuration or malformed/non-monotonic ASR input.
#[derive(Clone, Debug, Error, PartialEq)]
pub enum ConsensusError {
    #[error("history_size must be between 2 and 3")]
    InvalidHistorySize,
    #[error("agreement_passes must be between 2 and history_size")]
    InvalidAgreementPasses,
    #[error("pass window start must not exceed its end")]
    InvalidWindow,
    #[error("pass window_end_ms must increase monotonically")]
    NonIncreasingPass,
    #[error("hypothesis {hypothesis} has a word with invalid timestamps")]
    InvalidWordTimestamps { hypothesis: usize },
    #[error("hypothesis {hypothesis} word timestamps are not monotonic")]
    NonMonotonicWords { hypothesis: usize },
    #[error("hypothesis {hypothesis} contains an invalid probability")]
    InvalidProbability { hypothesis: usize },
    #[error("hypothesis {hypothesis} contains a non-finite ASR score")]
    InvalidScore { hypothesis: usize },
    #[error("the selected ambiguity resolution is not supported by recent ASR hypotheses")]
    UnsupportedResolution,
}

/// Stateful consensus tracker for one continuous dictation stream.
#[derive(Debug)]
pub struct RollingConsensus {
    config: ConsensusConfig,
    history: VecDeque<Pass>,
    committed_words: Vec<TimedWord>,
    committed_text: String,
}

impl RollingConsensus {
    /// Creates a tracker after validating its bounded-history configuration.
    ///
    /// # Errors
    ///
    /// Returns an error unless history is in `2..=3` and agreement requires at
    /// least two, but no more than the retained number of, passes.
    pub fn new(config: ConsensusConfig) -> Result<Self, ConsensusError> {
        validate_config(config)?;
        Ok(Self {
            config,
            history: VecDeque::with_capacity(config.history_size),
            committed_words: Vec::new(),
            committed_text: String::new(),
        })
    }

    /// Validates and observes one newer rolling ASR pass.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid windows, relative/out-of-window timestamps,
    /// invalid scores, or a pass that does not advance the live edge.
    pub fn observe(&mut self, pass: Pass) -> Result<ConsensusUpdate, ConsensusError> {
        validate_pass(&pass)?;
        if self
            .history
            .back()
            .is_some_and(|previous| pass.window_end_ms <= previous.window_end_ms)
        {
            return Err(ConsensusError::NonIncreasingPass);
        }
        if self.history.len() == self.config.history_size {
            self.history.pop_front();
        }
        self.history.push_back(pass);

        let previous_committed_len = self.committed_text.len();
        if self.history.len() >= self.config.agreement_passes {
            let ready = self.committable_words();
            for word in ready {
                append_word(&mut self.committed_text, &word.text);
                self.committed_words.push(word);
            }
        }

        let committed_append = self.committed_text[previous_committed_len..].to_owned();
        Ok(self.current_update(committed_append))
    }

    /// Commits the strongest remaining ASR hypothesis without LLM rewriting.
    ///
    /// This is intended for a VAD/explicit segment boundary. Cross-pass support
    /// chooses the hypothesis; ASR score and rank break ties. The history is
    /// cleared after finalization so the next segment cannot agree with stale
    /// alternatives.
    pub fn finalize(&mut self) -> ConsensusUpdate {
        let previous_committed_len = self.committed_text.len();
        if let Some(selection) = self.select_hypotheses() {
            let recent = self.recent_passes();
            if let Some(latest) = recent.get(selection.anchor_pass) {
                let words = self.pending_words(
                    latest,
                    &latest.hypotheses[selection.hypothesis_indices[selection.anchor_pass]],
                );
                let words = words.to_vec();
                for word in words {
                    append_word(&mut self.committed_text, &word.text);
                    self.committed_words.push(word);
                }
            }
        }
        self.history.clear();
        ConsensusUpdate {
            committed_append: self.committed_text[previous_committed_len..].to_owned(),
            committed_text: self.committed_text.clone(),
            best_unstable_text: String::new(),
            ambiguities: Vec::new(),
        }
    }

    /// Commits one mature wording reported by [`ConsensusUpdate::ambiguities`].
    ///
    /// `selected_text` must exactly match a recent acoustically supported
    /// candidate. `display_text` may contain separately validated surface
    /// normalization (for example, `pie torch` -> `PyTorch`); acoustic alignment
    /// continues to use the original selected words.
    ///
    /// # Errors
    ///
    /// Returns [`ConsensusError::UnsupportedResolution`] unless `selected_text`
    /// exactly matches a mature hypothesis in the retained ASR history.
    pub fn resolve_ambiguity(
        &mut self,
        selected_text: &str,
        display_text: &str,
    ) -> Result<ConsensusUpdate, ConsensusError> {
        let Some(latest) = self.history.back() else {
            return Err(ConsensusError::UnsupportedResolution);
        };
        let mature_cutoff = latest
            .window_end_ms
            .saturating_sub(self.config.unstable_tail_ms);
        let selected_words = self
            .history
            .iter()
            .rev()
            .flat_map(|pass| {
                pass.hypotheses.iter().map(|hypothesis| {
                    mature_prefix(self.pending_words(pass, hypothesis), mature_cutoff)
                })
            })
            .find(|words| render_words(words) == selected_text && !words.is_empty())
            .map(<[TimedWord]>::to_vec)
            .ok_or(ConsensusError::UnsupportedResolution)?;

        let previous_committed_len = self.committed_text.len();
        append_word(&mut self.committed_text, display_text);
        self.committed_words.extend(selected_words);
        Ok(self.current_update(self.committed_text[previous_committed_len..].to_owned()))
    }

    /// Complete immutable transcript prefix.
    #[must_use]
    pub fn committed_text(&self) -> &str {
        &self.committed_text
    }

    /// Number of retained ASR passes, exposed for diagnostics/tests.
    #[must_use]
    pub fn history_len(&self) -> usize {
        self.history.len()
    }

    /// Clears both committed transcript and ASR history.
    pub fn reset(&mut self) {
        self.history.clear();
        self.committed_words.clear();
        self.committed_text.clear();
    }

    fn current_update(&self, committed_append: String) -> ConsensusUpdate {
        let best_unstable_text = self
            .select_hypotheses()
            .map_or_else(String::new, |selection| {
                let recent = self.recent_passes();
                let latest = recent[selection.anchor_pass];
                let hypothesis =
                    &latest.hypotheses[selection.hypothesis_indices[selection.anchor_pass]];
                self.render_pending_append(latest, hypothesis)
            });
        let ambiguities = if self.history.len() >= self.config.agreement_passes {
            self.ambiguities()
        } else {
            Vec::new()
        };
        ConsensusUpdate {
            committed_append,
            committed_text: self.committed_text.clone(),
            best_unstable_text,
            ambiguities,
        }
    }

    fn committable_words(&self) -> Vec<TimedWord> {
        let Some(selection) = self.select_hypotheses() else {
            return Vec::new();
        };
        let recent = self.recent_passes();
        let anchor_pass = recent[selection.anchor_pass];
        let anchor = self.pending_words(
            anchor_pass,
            &anchor_pass.hypotheses[selection.hypothesis_indices[selection.anchor_pass]],
        );
        let mature_cutoff = anchor_pass
            .window_end_ms
            .saturating_sub(self.config.unstable_tail_ms);
        let mut aligned = Vec::with_capacity(selection.anchor_pass);
        for (pass_index, pass) in recent.iter().enumerate() {
            if pass_index == selection.anchor_pass {
                continue;
            }
            let other = self.pending_words(
                pass,
                &pass.hypotheses[selection.hypothesis_indices[pass_index]],
            );
            aligned.push((
                other,
                align_words(anchor, other, self.config.alignment_tolerance_ms),
            ));
        }

        let mut ready = Vec::new();
        for (anchor_index, word) in anchor.iter().enumerate() {
            if word.end_ms > mature_cutoff || lexical_key(&word.text).is_empty() {
                break;
            }
            let agrees = aligned.iter().all(|(other, mapping)| {
                mapping[anchor_index].is_some_and(|other_index| {
                    other_index == anchor_index
                        && other[other_index].end_ms <= mature_cutoff
                        && words_match(
                            word,
                            &other[other_index],
                            self.config.alignment_tolerance_ms,
                        )
                })
            });
            if !agrees {
                break;
            }
            ready.push(word.clone());
        }
        ready
    }

    fn ambiguities(&self) -> Vec<AmbiguousSpan> {
        let Some(selection) = self.select_hypotheses() else {
            return Vec::new();
        };
        let recent = self.recent_passes();
        let anchor_pass = recent[selection.anchor_pass];
        let mature_cutoff = anchor_pass
            .window_end_ms
            .saturating_sub(self.config.unstable_tail_ms);

        let selected_mature: Vec<&[TimedWord]> = recent
            .iter()
            .enumerate()
            .map(|(index, pass)| {
                let hypothesis = &pass.hypotheses[selection.hypothesis_indices[index]];
                mature_prefix(self.pending_words(pass, hypothesis), mature_cutoff)
            })
            .collect();
        let mut selected_keys = selected_mature.iter().map(|words| sequence_key(words));
        let Some(first_key) = selected_keys.next() else {
            return Vec::new();
        };
        if selected_keys.all(|key| key == first_key) {
            return Vec::new();
        }

        let start_ms = selected_mature
            .iter()
            .filter_map(|words| words.first().map(|word| word.start_ms))
            .min()
            .unwrap_or(mature_cutoff);
        let end_ms = selected_mature
            .iter()
            .filter_map(|words| words.last().map(|word| word.end_ms))
            .max()
            .unwrap_or(mature_cutoff);
        let mut accumulated: Vec<AccumulatedCandidate> = Vec::new();

        // Visit newest/highest-ranked alternatives first so their punctuation
        // and capitalization become the representative display text.
        for (pass_index, pass) in recent.iter().enumerate().rev() {
            for (rank, hypothesis) in pass.hypotheses.iter().enumerate() {
                let words = mature_prefix(self.pending_words(pass, hypothesis), mature_cutoff);
                let key = sequence_key(words);
                let text = render_words(words);
                let score = hypothesis.normalized_log_probability;
                let mean_probability = mean_word_probability(words);
                if let Some(existing) = accumulated.iter_mut().find(|item| item.key == key) {
                    existing.evidence.hypothesis_support += 1;
                    if !existing.pass_indices.contains(&pass_index) {
                        existing.pass_indices.push(pass_index);
                        existing.evidence.pass_support += 1;
                    }
                    existing.evidence.best_rank = existing.evidence.best_rank.min(rank);
                    existing.evidence.best_normalized_log_probability =
                        max_score(existing.evidence.best_normalized_log_probability, score);
                    existing.evidence.best_mean_word_probability = max_score(
                        existing.evidence.best_mean_word_probability,
                        mean_probability,
                    );
                } else {
                    accumulated.push(AccumulatedCandidate {
                        key,
                        pass_indices: vec![pass_index],
                        evidence: CandidateEvidence {
                            text,
                            pass_support: 1,
                            hypothesis_support: 1,
                            best_rank: rank,
                            best_normalized_log_probability: score,
                            best_mean_word_probability: mean_probability,
                        },
                    });
                }
            }
        }
        if accumulated.len() < 2 {
            return Vec::new();
        }
        let mut candidates: Vec<CandidateEvidence> = accumulated
            .into_iter()
            .map(|candidate| candidate.evidence)
            .collect();
        candidates.sort_by(compare_candidate_evidence);
        vec![AmbiguousSpan {
            start_ms,
            end_ms,
            candidates,
        }]
    }

    fn recent_passes(&self) -> Vec<&Pass> {
        let count = self.config.agreement_passes.min(self.history.len());
        self.history
            .iter()
            .skip(self.history.len() - count)
            .collect()
    }

    fn select_hypotheses(&self) -> Option<Selection> {
        let recent = self.recent_passes();
        let anchor_pass = recent.len().checked_sub(1)?;
        let anchor = recent[anchor_pass];
        if anchor.hypotheses.is_empty() {
            return None;
        }

        let mut best: Option<SelectionScore> = None;
        for (anchor_index, anchor_hypothesis) in anchor.hypotheses.iter().enumerate() {
            let anchor_words = self.pending_words(anchor, anchor_hypothesis);
            let mut indices = vec![0; recent.len()];
            indices[anchor_pass] = anchor_index;
            let mut matching_passes = 0;
            let mut total_matches = 0;

            for (pass_index, pass) in recent[..anchor_pass].iter().enumerate() {
                let mut best_other: Option<(usize, usize, Option<f32>)> = None;
                for (hypothesis_index, hypothesis) in pass.hypotheses.iter().enumerate() {
                    let other_words = self.pending_words(pass, hypothesis);
                    let matches = align_words(
                        anchor_words,
                        other_words,
                        self.config.alignment_tolerance_ms,
                    )
                    .into_iter()
                    .flatten()
                    .count();
                    let contender = (
                        hypothesis_index,
                        matches,
                        hypothesis.normalized_log_probability,
                    );
                    if best_other
                        .as_ref()
                        .is_none_or(|current| better_other(contender, *current))
                    {
                        best_other = Some(contender);
                    }
                }
                if let Some((hypothesis_index, matches, _)) = best_other {
                    indices[pass_index] = hypothesis_index;
                    total_matches += matches;
                    matching_passes += usize::from(matches > 0 || anchor_words.is_empty());
                }
            }
            let contender = SelectionScore {
                selection: Selection {
                    anchor_pass,
                    hypothesis_indices: indices,
                },
                matching_passes,
                total_matches,
                anchor_score: anchor_hypothesis.normalized_log_probability,
                anchor_rank: anchor_index,
            };
            if best
                .as_ref()
                .is_none_or(|current| contender.better_than(current))
            {
                best = Some(contender);
            }
        }
        best.map(|score| score.selection)
    }

    fn pending_words<'a>(&self, pass: &Pass, hypothesis: &'a Hypothesis) -> &'a [TimedWord] {
        if self.committed_words.is_empty() {
            return &hypothesis.words;
        }
        let committed_end = self.committed_words.last().map_or(0, |word| word.end_ms);
        let by_time = hypothesis
            .words
            .partition_point(|word| word.end_ms <= committed_end);
        let committed_start = self
            .committed_words
            .partition_point(|word| {
                word.end_ms
                    .saturating_add(self.config.alignment_tolerance_ms)
                    < pass.window_start_ms
            })
            .max(
                self.committed_words
                    .len()
                    .saturating_sub(COMMITTED_ALIGNMENT_WORDS),
            );
        let mapping = align_words(
            &self.committed_words[committed_start..],
            &hypothesis.words,
            self.config.alignment_tolerance_ms,
        );
        let after_alignment = mapping
            .into_iter()
            .flatten()
            .max()
            .map_or(0, |index| index + 1);
        &hypothesis.words[by_time.max(after_alignment)..]
    }

    fn render_pending_append(&self, pass: &Pass, hypothesis: &Hypothesis) -> String {
        let mut text = self.committed_text.clone();
        let prefix_len = text.len();
        for word in self.pending_words(pass, hypothesis) {
            append_word(&mut text, &word.text);
        }
        text[prefix_len..].to_owned()
    }
}

#[derive(Clone, Debug)]
struct Selection {
    anchor_pass: usize,
    hypothesis_indices: Vec<usize>,
}

#[derive(Clone, Debug)]
struct SelectionScore {
    selection: Selection,
    matching_passes: usize,
    total_matches: usize,
    anchor_score: Option<f32>,
    anchor_rank: usize,
}

impl SelectionScore {
    fn better_than(&self, other: &Self) -> bool {
        self.matching_passes
            .cmp(&other.matching_passes)
            .then_with(|| self.total_matches.cmp(&other.total_matches))
            .then_with(|| compare_optional_score(self.anchor_score, other.anchor_score))
            .then_with(|| other.anchor_rank.cmp(&self.anchor_rank))
            == Ordering::Greater
    }
}

#[derive(Debug)]
struct AccumulatedCandidate {
    key: String,
    pass_indices: Vec<usize>,
    evidence: CandidateEvidence,
}

fn validate_config(config: ConsensusConfig) -> Result<(), ConsensusError> {
    if !(MIN_HISTORY_SIZE..=MAX_HISTORY_SIZE).contains(&config.history_size) {
        return Err(ConsensusError::InvalidHistorySize);
    }
    if !(MIN_HISTORY_SIZE..=config.history_size).contains(&config.agreement_passes) {
        return Err(ConsensusError::InvalidAgreementPasses);
    }
    Ok(())
}

fn validate_pass(pass: &Pass) -> Result<(), ConsensusError> {
    if pass.window_start_ms > pass.window_end_ms {
        return Err(ConsensusError::InvalidWindow);
    }
    for (hypothesis_index, hypothesis) in pass.hypotheses.iter().enumerate() {
        if hypothesis
            .normalized_log_probability
            .is_some_and(|score| !score.is_finite())
        {
            return Err(ConsensusError::InvalidScore {
                hypothesis: hypothesis_index,
            });
        }
        let mut previous_start = None;
        let mut previous_end = None;
        for word in &hypothesis.words {
            if word.start_ms > word.end_ms
                || word.start_ms < pass.window_start_ms
                || word.end_ms > pass.window_end_ms
            {
                return Err(ConsensusError::InvalidWordTimestamps {
                    hypothesis: hypothesis_index,
                });
            }
            if word.probability.is_some_and(|probability| {
                !probability.is_finite() || !(0.0..=1.0).contains(&probability)
            }) {
                return Err(ConsensusError::InvalidProbability {
                    hypothesis: hypothesis_index,
                });
            }
            if previous_start.is_some_and(|start| word.start_ms < start)
                || previous_end.is_some_and(|end| word.end_ms < end)
            {
                return Err(ConsensusError::NonMonotonicWords {
                    hypothesis: hypothesis_index,
                });
            }
            previous_start = Some(word.start_ms);
            previous_end = Some(word.end_ms);
        }
    }
    Ok(())
}

fn align_words(left: &[TimedWord], right: &[TimedWord], tolerance_ms: u64) -> Vec<Option<usize>> {
    let rows = left.len() + 1;
    let columns = right.len() + 1;
    let mut scores = vec![0_usize; rows * columns];
    let at = |row: usize, column: usize| row * columns + column;
    for row in 1..rows {
        for column in 1..columns {
            scores[at(row, column)] =
                if words_match(&left[row - 1], &right[column - 1], tolerance_ms) {
                    (scores[at(row - 1, column - 1)] + 1)
                        .max(scores[at(row - 1, column)])
                        .max(scores[at(row, column - 1)])
                } else {
                    scores[at(row - 1, column)].max(scores[at(row, column - 1)])
                };
        }
    }

    let mut mapping = vec![None; left.len()];
    let (mut row, mut column) = (left.len(), right.len());
    while row > 0 && column > 0 {
        if words_match(&left[row - 1], &right[column - 1], tolerance_ms)
            && scores[at(row, column)] == scores[at(row - 1, column - 1)] + 1
        {
            mapping[row - 1] = Some(column - 1);
            row -= 1;
            column -= 1;
        } else if scores[at(row - 1, column)] >= scores[at(row, column - 1)] {
            row -= 1;
        } else {
            column -= 1;
        }
    }
    mapping
}

fn words_match(left: &TimedWord, right: &TimedWord, tolerance_ms: u64) -> bool {
    let left_key = lexical_key(&left.text);
    !left_key.is_empty()
        && left_key == lexical_key(&right.text)
        && midpoint(left).abs_diff(midpoint(right)) <= tolerance_ms
}

fn midpoint(word: &TimedWord) -> u64 {
    word.start_ms + word.end_ms.saturating_sub(word.start_ms) / 2
}

fn lexical_key(text: &str) -> String {
    text.trim()
        .trim_matches(is_edge_formatting)
        .chars()
        .flat_map(char::to_lowercase)
        .collect()
}

fn is_edge_formatting(character: char) -> bool {
    matches!(
        character,
        '.' | ','
            | '!'
            | '?'
            | ';'
            | ':'
            | '"'
            | '\''
            | '“'
            | '”'
            | '‘'
            | '’'
            | '('
            | ')'
            | '['
            | ']'
            | '{'
            | '}'
    )
}

fn mature_prefix(words: &[TimedWord], cutoff_ms: u64) -> &[TimedWord] {
    &words[..words.partition_point(|word| word.end_ms <= cutoff_ms)]
}

fn sequence_key(words: &[TimedWord]) -> String {
    words
        .iter()
        .map(|word| lexical_key(&word.text))
        .filter(|key| !key.is_empty())
        .collect::<Vec<_>>()
        .join("\u{1f}")
}

fn mean_word_probability(words: &[TimedWord]) -> Option<f32> {
    let (sum, count) = words
        .iter()
        .filter_map(|word| word.probability)
        .fold((0.0_f32, 0.0_f32), |(sum, count), probability| {
            (sum + probability, count + 1.0)
        });
    (count > 0.0).then(|| sum / count)
}

fn better_other(
    contender: (usize, usize, Option<f32>),
    current: (usize, usize, Option<f32>),
) -> bool {
    contender
        .1
        .cmp(&current.1)
        .then_with(|| compare_optional_score(contender.2, current.2))
        .then_with(|| current.0.cmp(&contender.0))
        == Ordering::Greater
}

fn compare_optional_score(left: Option<f32>, right: Option<f32>) -> Ordering {
    match (left, right) {
        (Some(left), Some(right)) => left.total_cmp(&right),
        (Some(_), None) => Ordering::Greater,
        (None, Some(_)) => Ordering::Less,
        (None, None) => Ordering::Equal,
    }
}

fn max_score(left: Option<f32>, right: Option<f32>) -> Option<f32> {
    if compare_optional_score(left, right) == Ordering::Less {
        right
    } else {
        left
    }
}

fn compare_candidate_evidence(left: &CandidateEvidence, right: &CandidateEvidence) -> Ordering {
    right
        .pass_support
        .cmp(&left.pass_support)
        .then_with(|| right.hypothesis_support.cmp(&left.hypothesis_support))
        .then_with(|| {
            compare_optional_score(
                right.best_normalized_log_probability,
                left.best_normalized_log_probability,
            )
        })
        .then_with(|| left.best_rank.cmp(&right.best_rank))
        .then_with(|| left.text.cmp(&right.text))
}

fn render_words(words: &[TimedWord]) -> String {
    let mut text = String::new();
    for word in words {
        append_word(&mut text, &word.text);
    }
    text
}

fn append_word(output: &mut String, word: &str) {
    if word.is_empty() {
        return;
    }
    let starts_with_space = word.chars().next().is_some_and(char::is_whitespace);
    let output_ends_with_space = output.chars().next_back().is_some_and(char::is_whitespace);
    let first = word.trim_start().chars().next();
    let previous = output.chars().next_back();
    let attaches_left = first.is_some_and(|character| {
        matches!(
            character,
            '.' | ',' | '!' | '?' | ';' | ':' | ')' | ']' | '}'
        )
    });
    let opens_group = previous.is_some_and(|character| matches!(character, '(' | '[' | '{'));
    if !output.is_empty()
        && !starts_with_space
        && !output_ends_with_space
        && !attaches_left
        && !opens_group
    {
        output.push(' ');
    }
    output.push_str(word);
}
