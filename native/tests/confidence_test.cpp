#include "openflow/inference/confidence.hpp"

#include <cmath>
#include <iostream>
#include <string>
#include <vector>

namespace {

using namespace openflow::inference;

int failures = 0;

void expect(bool condition, const std::string& message) {
  if (!condition) {
    std::cerr << "FAILED: " << message << '\n';
    ++failures;
  }
}

EditDecision whole_sentence_decision(const std::string& raw, const std::string& proposal,
                                     EditKind kind, double confidence,
                                     double llm_delta_nats_per_token,
                                     bool mark_protected = false) {
  std::vector<WordEvidence> words{
      WordEvidence{raw, 0, raw.size(), confidence, mark_protected || looks_protected(raw)}};
  constexpr std::size_t token_count = 4;
  return gate_edit(raw, words, {}, EditCandidate{0, raw.size(), proposal, kind}, -10.0,
                   -10.0 + llm_delta_nats_per_token * token_count, token_count);
}

void regression_policy_cases() {
  // Mirrors fixtures/confidence-gates.json without making the native package depend
  // on a repository-relative runtime path.
  expect(whole_sentence_decision("lets meet at five", "Let's meet at five.",
                                 EditKind::kFormatting, 0.98, -0.1)
             .accepted,
         "formatting is safe");
  expect(!whole_sentence_decision("deploy to production", "deploy into production",
                                  EditKind::kLexical, 0.91, 2.2)
              .accepted,
         "high-confidence lexical edit is blocked");
  expect(whole_sentence_decision("ouvrez le dépôt gitte", "ouvrez le dépôt Git",
                                 EditKind::kLexical, 0.52, 0.72)
             .accepted,
         "medium-confidence edit with sufficient margin is accepted");
  expect(!whole_sentence_decision("abre el archivo local", "abre un archivo local",
                                  EditKind::kLexical, 0.48, 0.2)
              .accepted,
         "medium-confidence edit with weak margin is blocked");
  expect(whole_sentence_decision("öffne die kontainer datei", "öffne die Container-Datei",
                                 EditKind::kLexical, 0.21, 0.08)
             .accepted,
         "low-confidence edit with nonnegative margin is accepted");
  expect(!whole_sentence_decision("send 150 dollars", "send 50 dollars", EditKind::kLexical,
                                  0.1, 4.0)
              .accepted,
         "numeric text is always protected");
  expect(whole_sentence_decision("設定を開いてください", "設定を開いてください。",
                                 EditKind::kFormatting, 0.97, 0.0)
             .accepted,
         "unicode punctuation is safe");
}

void aggregation_cases() {
  const auto words = aggregate_word_confidence(
      {{" hello", 0.81}, {" wor", 0.25}, {"ld", 1.0}, {" 150", 0.1}});
  expect(words.size() == 3, "token pieces aggregate into three words");
  expect(words.size() > 0 && words[0].text == "hello" &&
             std::abs(words[0].confidence - 0.81) < 1e-9,
         "single-token word keeps its probability");
  expect(words.size() > 1 && words[1].text == "world" &&
             std::abs(words[1].confidence - 0.5) < 1e-9,
         "multi-token word uses geometric mean probability");
  expect(words.size() > 2 && words[2].protected_text, "numeric word is protected");
}

void validation_cases() {
  const std::string text = "a café here";
  const auto words = aggregate_word_confidence({{text, 0.2}});
  const auto invalid_utf8 = gate_edit(text, words, {}, EditCandidate{6, 7, "x", EditKind::kLexical},
                                      -2.0, 2.0, 1);
  expect(!invalid_utf8.accepted && invalid_utf8.reason == "invalid_range",
         "edits cannot split UTF-8 code points");
  const auto protected_range = gate_edit(
      text, words, {{2, 7}}, EditCandidate{2, 7, "tea", EditKind::kLexical}, -2.0, 2.0, 1);
  expect(!protected_range.accepted && protected_range.reason == "protected_range",
         "explicit protected ranges override model scores");
  expect(apply_edits("hello world", {{5, 5, ",", EditKind::kFormatting},
                                      {11, 11, "!", EditKind::kFormatting}}) == "hello, world!",
         "non-overlapping edits apply against original byte offsets");
  const std::vector<WordEvidence> duplicate_words{{"hello", 0, 5, 0.9, false},
                                                   {"hello", 6, 11, 0.9, false}};
  const auto duplicate = gate_edit("hello hello", duplicate_words, {},
                                   EditCandidate{5, 11, "", EditKind::kAdjacentDuplicate},
                                   -3.0, -1.0, 1);
  expect(duplicate.accepted, "verified adjacent duplicate bypasses the length-change cap");
  const auto mislabeled = gate_edit("hello", {{"hello", 0, 5, 0.1, false}}, {},
                                    EditCandidate{0, 5, "goodbye", EditKind::kFormatting},
                                    -3.0, 3.0, 1);
  expect(!mislabeled.accepted && mislabeled.reason == "invalid_formatting_edit",
         "lexical replacement cannot bypass gates by claiming formatting kind");
}

}  // namespace

int main() {
  regression_policy_cases();
  aggregation_cases();
  validation_cases();
  if (failures != 0) std::cerr << failures << " confidence test(s) failed\n";
  return failures == 0 ? 0 : 1;
}
