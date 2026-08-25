#include "openflow/inference/confidence.hpp"

#include <algorithm>
#include <cctype>
#include <cmath>
#include <limits>
#include <stdexcept>

namespace openflow::inference {
namespace {

constexpr double kMinimumProbability = 1e-9;

bool overlaps(std::size_t left_start, std::size_t left_end, std::size_t right_start,
              std::size_t right_end) {
  if (left_start == left_end) return left_start > right_start && left_start < right_end;
  if (right_start == right_end) return right_start > left_start && right_start < left_end;
  return left_start < right_end && right_start < left_end;
}

bool is_utf8_boundary(const std::string& value, std::size_t offset) {
  return offset == value.size() ||
         (offset < value.size() &&
          (static_cast<unsigned char>(value[offset]) & static_cast<unsigned char>(0xc0)) != 0x80);
}

std::string lowercase_ascii(std::string value) {
  std::transform(value.begin(), value.end(), value.begin(), [](unsigned char character) {
    return static_cast<char>(std::tolower(character));
  });
  return value;
}

std::string lexical_skeleton(const std::string& value) {
  std::string output;
  for (std::size_t index = 0; index < value.size();) {
    const std::string tail = value.substr(index);
    const std::vector<std::string> unicode_punctuation{"…", "。", "！", "？", "、"};
    const auto punctuation = std::find_if(
        unicode_punctuation.begin(), unicode_punctuation.end(),
        [&tail](const std::string& candidate) { return tail.rfind(candidate, 0) == 0; });
    if (punctuation != unicode_punctuation.end()) {
      index += punctuation->size();
      continue;
    }
    const unsigned char character = static_cast<unsigned char>(value[index++]);
    if (character >= 0x80U || std::isalnum(character) != 0) {
      output.push_back(static_cast<char>(character >= 'A' && character <= 'Z'
                                             ? character + ('a' - 'A')
                                             : character));
    }
  }
  return output;
}

bool safe_formatting_change(const std::string& source, const std::string& replacement) {
  if (source.empty()) {
    static const std::vector<std::string> allowed_insertions{
        ".", ",", "!", "?", ";", ":", "\n", "…", "。", "！", "？", "、"};
    return std::find(allowed_insertions.begin(), allowed_insertions.end(), replacement) !=
           allowed_insertions.end();
  }
  return lexical_skeleton(source) == lexical_skeleton(replacement);
}

bool exact_adjacent_duplicate(const std::string& transcript, const EditCandidate& candidate) {
  if (!candidate.replacement.empty() || candidate.start_byte >= candidate.end_byte ||
      candidate.end_byte > transcript.size()) return false;
  std::size_t duplicate_start = candidate.start_byte;
  while (duplicate_start < candidate.end_byte &&
         std::isspace(static_cast<unsigned char>(transcript[duplicate_start])) != 0) {
    ++duplicate_start;
  }
  if (duplicate_start == candidate.start_byte || duplicate_start == candidate.end_byte) return false;
  const std::string duplicate = transcript.substr(duplicate_start, candidate.end_byte - duplicate_start);
  if (std::any_of(duplicate.begin(), duplicate.end(), [](unsigned char character) {
        return std::isspace(character) != 0;
      })) return false;
  std::size_t previous_end = candidate.start_byte;
  while (previous_end > 0 &&
         std::isspace(static_cast<unsigned char>(transcript[previous_end - 1])) != 0) {
    --previous_end;
  }
  std::size_t previous_start = previous_end;
  while (previous_start > 0 &&
         std::isspace(static_cast<unsigned char>(transcript[previous_start - 1])) == 0) {
    --previous_start;
  }
  if (previous_start == previous_end) return false;
  return lowercase_ascii(transcript.substr(previous_start, previous_end - previous_start)) ==
         lowercase_ascii(duplicate);
}

std::size_t word_count(const std::string& value) {
  std::size_t count = 0;
  bool in_word = false;
  for (const unsigned char character : value) {
    const bool whitespace = std::isspace(character) != 0;
    if (!whitespace && !in_word) ++count;
    in_word = !whitespace;
  }
  return count;
}

bool normalization_kind(EditKind kind) {
  return kind == EditKind::kFormatting || kind == EditKind::kWordBoundary ||
         kind == EditKind::kOrthographicNormalization || kind == EditKind::kCanonicalName ||
         kind == EditKind::kSpokenSymbol;
}

bool grounding_matches_kind(EditKind kind, NormalizationGrounding grounding) {
  switch (kind) {
    case EditKind::kFormatting:
    case EditKind::kWordBoundary:
      return grounding == NormalizationGrounding::kLexicalSkeleton;
    case EditKind::kOrthographicNormalization:
      return grounding == NormalizationGrounding::kLexicalSkeleton ||
             grounding == NormalizationGrounding::kPhoneticEquivalence;
    case EditKind::kCanonicalName:
      return grounding == NormalizationGrounding::kPhoneticEquivalence ||
             grounding == NormalizationGrounding::kCanonicalAlias;
    case EditKind::kSpokenSymbol:
      return grounding == NormalizationGrounding::kSpokenSymbol;
    case EditKind::kAdjacentDuplicate:
    case EditKind::kLexical:
      return false;
  }
  return false;
}

}  // namespace

std::vector<WordEvidence> aggregate_word_confidence(const std::vector<TokenEvidence>& tokens) {
  std::vector<WordEvidence> words;
  std::string transcript;
  std::string current;
  std::size_t current_start = 0;
  double log_probability_sum = 0.0;
  std::size_t evidence_count = 0;

  auto finish_word = [&]() {
    if (current.empty()) return;
    const double confidence = std::exp(log_probability_sum / static_cast<double>(evidence_count));
    words.push_back(
        WordEvidence{current, current_start, transcript.size(), confidence, looks_protected(current)});
    current.clear();
    log_probability_sum = 0.0;
    evidence_count = 0;
  };

  for (const auto& token : tokens) {
    const double probability = std::clamp(token.probability, kMinimumProbability, 1.0);
    bool probability_added_to_current = false;
    for (const unsigned char character : token.text) {
      if (std::isspace(character) != 0) {
        finish_word();
        probability_added_to_current = false;
        transcript.push_back(static_cast<char>(character));
        continue;
      }
      if (current.empty()) current_start = transcript.size();
      if (!probability_added_to_current) {
        log_probability_sum += std::log(probability);
        ++evidence_count;
        probability_added_to_current = true;
      }
      current.push_back(static_cast<char>(character));
      transcript.push_back(static_cast<char>(character));
    }
  }
  finish_word();
  return words;
}

bool looks_protected(const std::string& text) {
  if (text.empty()) return false;
  if (text.find("://") != std::string::npos || text.find("www.") != std::string::npos ||
      text.find('@') != std::string::npos || text.find('/') != std::string::npos ||
      text.find('\\') != std::string::npos) return true;
  if (std::any_of(text.begin(), text.end(), [](unsigned char character) {
        return std::isdigit(character) != 0;
      })) return true;
  if (text.find('_') != std::string::npos || text.find("::") != std::string::npos ||
      text.find("->") != std::string::npos || text.find("()") != std::string::npos) return true;
  const std::string lower = lowercase_ascii(text);
  return lower.rfind("0x", 0) == 0;
}

EditDecision gate_edit(const std::string& transcript, const std::vector<WordEvidence>& words,
                       const std::vector<ProtectedRange>& protected_ranges,
                       const EditCandidate& candidate, double original_log_probability,
                       double proposed_log_probability, std::size_t source_token_count,
                       const CleanupPolicy& policy) {
  EditDecision decision;
  if (candidate.start_byte > candidate.end_byte || candidate.end_byte > transcript.size() ||
      !is_utf8_boundary(transcript, candidate.start_byte) ||
      !is_utf8_boundary(transcript, candidate.end_byte)) {
    decision.reason = "invalid_range";
    return decision;
  }
  if (candidate.start_byte == candidate.end_byte && candidate.replacement.empty()) {
    decision.reason = "no_change";
    return decision;
  }
  if (transcript.substr(candidate.start_byte, candidate.end_byte - candidate.start_byte) ==
      candidate.replacement) {
    decision.reason = "no_change";
    return decision;
  }
  if (candidate.kind == EditKind::kWordBoundary ||
      candidate.kind == EditKind::kOrthographicNormalization ||
      candidate.kind == EditKind::kCanonicalName || candidate.kind == EditKind::kSpokenSymbol) {
    decision.reason = "normalization_requires_external_evidence";
    return decision;
  }
  for (const auto& range : protected_ranges) {
    if (range.start_byte > range.end_byte || range.end_byte > transcript.size()) {
      decision.reason = "invalid_protected_range";
      return decision;
    }
    if (overlaps(candidate.start_byte, candidate.end_byte, range.start_byte, range.end_byte)) {
      decision.reason = "protected_range";
      return decision;
    }
  }

  std::vector<double> confidences;
  for (const auto& word : words) {
    if (!overlaps(candidate.start_byte, candidate.end_byte, word.start_byte, word.end_byte)) continue;
    if (word.protected_text) {
      decision.reason = "protected_text";
      return decision;
    }
    confidences.push_back(std::clamp(word.confidence, kMinimumProbability, 1.0));
  }
  if (confidences.empty()) {
    decision.source_confidence = candidate.kind == EditKind::kLexical ? 1.0 : 0.0;
  } else {
    double log_sum = 0.0;
    for (const double confidence : confidences) log_sum += std::log(confidence);
    decision.source_confidence = std::exp(log_sum / static_cast<double>(confidences.size()));
  }

  const std::string source =
      transcript.substr(candidate.start_byte, candidate.end_byte - candidate.start_byte);
  if (candidate.kind == EditKind::kFormatting &&
      !safe_formatting_change(source, candidate.replacement)) {
    decision.reason = "invalid_formatting_edit";
    return decision;
  }
  if (candidate.kind == EditKind::kAdjacentDuplicate) {
    if (!exact_adjacent_duplicate(transcript, candidate)) {
      decision.reason = "not_an_adjacent_duplicate";
      return decision;
    }
    decision.accepted = true;
    decision.reason = "adjacent_duplicate";
    return decision;
  }

  const std::size_t result_length = transcript.size() -
                                    (candidate.end_byte - candidate.start_byte) +
                                    candidate.replacement.size();
  const double relative_change =
      std::abs(static_cast<double>(result_length) - static_cast<double>(transcript.size())) /
      static_cast<double>(std::max<std::size_t>(1, transcript.size()));
  if (relative_change > policy.maximum_result_length_change_ratio) {
    decision.reason = "excessive_length_change";
    return decision;
  }

  if (candidate.kind == EditKind::kFormatting) {
    decision.accepted = true;
    decision.reason = "safe_formatting";
    return decision;
  }
  if (source_token_count == 0 || !std::isfinite(original_log_probability) ||
      !std::isfinite(proposed_log_probability)) {
    decision.reason = "missing_score";
    return decision;
  }

  decision.llm_advantage_nats_per_token =
      (proposed_log_probability - original_log_probability) /
      static_cast<double>(source_token_count);
  if (decision.source_confidence >= policy.high_confidence) {
    decision.reason = "high_asr_confidence";
    return decision;
  }
  const double threshold = decision.source_confidence >= policy.low_confidence
                               ? policy.medium_min_advantage_nats
                               : policy.low_min_advantage_nats;
  if (decision.llm_advantage_nats_per_token < threshold) {
    decision.reason = "insufficient_llm_advantage";
    return decision;
  }
  decision.accepted = true;
  decision.reason = "confidence_and_score_allow_edit";
  return decision;
}

std::string apply_edits(const std::string& transcript, std::vector<EditCandidate> accepted_edits) {
  std::sort(accepted_edits.begin(), accepted_edits.end(), [](const auto& left, const auto& right) {
    if (left.start_byte != right.start_byte) return left.start_byte < right.start_byte;
    return left.end_byte < right.end_byte;
  });
  std::size_t previous_end = 0;
  bool first = true;
  for (const auto& edit : accepted_edits) {
    if (edit.start_byte > edit.end_byte || edit.end_byte > transcript.size() ||
        (!first && edit.start_byte < previous_end)) {
      throw std::invalid_argument("cannot apply invalid or overlapping edits");
    }
    first = false;
    previous_end = edit.end_byte;
  }
  std::string result = transcript;
  for (auto iterator = accepted_edits.rbegin(); iterator != accepted_edits.rend(); ++iterator) {
    result.replace(iterator->start_byte, iterator->end_byte - iterator->start_byte,
                   iterator->replacement);
  }
  return result;
}

const char* edit_kind_name(EditKind kind) {
  switch (kind) {
    case EditKind::kFormatting: return "formatting";
    case EditKind::kAdjacentDuplicate: return "adjacent_duplicate";
    case EditKind::kLexical: return "lexical";
    case EditKind::kWordBoundary: return "word_boundary";
    case EditKind::kOrthographicNormalization: return "orthographic_normalization";
    case EditKind::kCanonicalName: return "canonical_name";
    case EditKind::kSpokenSymbol: return "spoken_symbol";
  }
  return "lexical";
}

EditKind parse_edit_kind(const std::string& value) {
  if (value == "formatting") return EditKind::kFormatting;
  if (value == "adjacent_duplicate") return EditKind::kAdjacentDuplicate;
  if (value == "lexical") return EditKind::kLexical;
  if (value == "word_boundary") return EditKind::kWordBoundary;
  if (value == "orthographic_normalization") return EditKind::kOrthographicNormalization;
  if (value == "canonical_name") return EditKind::kCanonicalName;
  if (value == "spoken_symbol") return EditKind::kSpokenSymbol;
  throw std::invalid_argument("unknown edit kind: " + value);
}

const char* normalization_grounding_name(NormalizationGrounding grounding) {
  switch (grounding) {
    case NormalizationGrounding::kLexicalSkeleton: return "lexical_skeleton";
    case NormalizationGrounding::kPhoneticEquivalence: return "phonetic_equivalence";
    case NormalizationGrounding::kCanonicalAlias: return "canonical_alias";
    case NormalizationGrounding::kSpokenSymbol: return "spoken_symbol";
  }
  return "lexical_skeleton";
}

NormalizationGrounding parse_normalization_grounding(const std::string& value) {
  if (value == "lexical_skeleton") return NormalizationGrounding::kLexicalSkeleton;
  if (value == "phonetic_equivalence") return NormalizationGrounding::kPhoneticEquivalence;
  if (value == "canonical_alias") return NormalizationGrounding::kCanonicalAlias;
  if (value == "spoken_symbol") return NormalizationGrounding::kSpokenSymbol;
  throw std::invalid_argument("unknown normalization grounding: " + value);
}

NormalizationValidation validate_normalization_proposal(
    const std::string& transcript, const EditCandidate& candidate,
    NormalizationGrounding grounding) {
  if (!normalization_kind(candidate.kind)) return {false, "disallowed_kind"};
  if (candidate.start_byte > candidate.end_byte || candidate.end_byte > transcript.size() ||
      !is_utf8_boundary(transcript, candidate.start_byte) ||
      !is_utf8_boundary(transcript, candidate.end_byte)) {
    return {false, "invalid_range"};
  }
  const std::string source =
      transcript.substr(candidate.start_byte, candidate.end_byte - candidate.start_byte);
  if (source == candidate.replacement) return {false, "no_change"};
  if (!grounding_matches_kind(candidate.kind, grounding)) {
    return {false, "incompatible_grounding"};
  }
  constexpr std::size_t kMaximumLocalBytes = 128;
  constexpr std::size_t kMaximumLocalWords = 4;
  if (source.size() > kMaximumLocalBytes || candidate.replacement.size() > kMaximumLocalBytes ||
      word_count(source) > kMaximumLocalWords ||
      word_count(candidate.replacement) > kMaximumLocalWords) {
    return {false, "nonlocal_normalization"};
  }
  if (candidate.kind == EditKind::kFormatting) {
    return safe_formatting_change(source, candidate.replacement)
               ? NormalizationValidation{true, "safe_formatting"}
               : NormalizationValidation{false, "invalid_formatting_edit"};
  }
  if (source.empty() || candidate.replacement.empty() || lexical_skeleton(source).empty() ||
      lexical_skeleton(candidate.replacement).empty()) {
    return {false, "unusable_source"};
  }
  if (looks_protected(source) || looks_protected(candidate.replacement)) {
    return {false, "protected_text"};
  }
  if (candidate.kind == EditKind::kWordBoundary &&
      lexical_skeleton(source) != lexical_skeleton(candidate.replacement)) {
    return {false, "invalid_word_boundary"};
  }
  return {true, "structured_normalization"};
}

}  // namespace openflow::inference
