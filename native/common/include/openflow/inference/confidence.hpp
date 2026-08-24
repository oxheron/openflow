#pragma once

#include <cstddef>
#include <string>
#include <vector>

namespace openflow::inference {

struct TokenEvidence {
  std::string text;
  double probability{0.0};
};

struct WordEvidence {
  std::string text;
  std::size_t start_byte{0};
  std::size_t end_byte{0};
  double confidence{0.0};
  bool protected_text{false};
};

enum class EditKind { kFormatting, kAdjacentDuplicate, kLexical };

struct EditCandidate {
  std::size_t start_byte{0};
  std::size_t end_byte{0};
  std::string replacement;
  EditKind kind{EditKind::kLexical};
};

struct ProtectedRange {
  std::size_t start_byte{0};
  std::size_t end_byte{0};
};

struct CleanupPolicy {
  double high_confidence{0.75};
  double low_confidence{0.35};
  double medium_min_advantage_nats{0.5};
  double low_min_advantage_nats{0.0};
  double maximum_result_length_change_ratio{0.25};
};

struct EditDecision {
  bool accepted{false};
  std::string reason;
  double source_confidence{0.0};
  double llm_advantage_nats_per_token{0.0};
};

[[nodiscard]] std::vector<WordEvidence> aggregate_word_confidence(
    const std::vector<TokenEvidence>& tokens);
[[nodiscard]] bool looks_protected(const std::string& text);
[[nodiscard]] EditDecision gate_edit(const std::string& transcript,
                                     const std::vector<WordEvidence>& words,
                                     const std::vector<ProtectedRange>& protected_ranges,
                                     const EditCandidate& candidate,
                                     double original_log_probability,
                                     double proposed_log_probability,
                                     std::size_t source_token_count,
                                     const CleanupPolicy& policy = {});
[[nodiscard]] std::string apply_edits(const std::string& transcript,
                                      std::vector<EditCandidate> accepted_edits);
[[nodiscard]] const char* edit_kind_name(EditKind kind);
[[nodiscard]] EditKind parse_edit_kind(const std::string& value);

}  // namespace openflow::inference
