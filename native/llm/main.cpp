#include "llm_backend.hpp"

#include "openflow/inference/confidence.hpp"
#include "openflow/inference/json.hpp"
#include "openflow/inference/worker.hpp"

#include <algorithm>
#include <cctype>
#include <cmath>
#include <iterator>
#include <memory>
#include <stdexcept>
#include <string>
#include <unordered_map>
#include <unordered_set>
#include <vector>

namespace openflow::inference::llm {
namespace {

using json::Value;

Value string_array(const std::vector<std::string>& values) {
  Value::Array output;
  for (const auto& value : values) output.emplace_back(value);
  return output;
}

std::vector<std::string> compiled_compute_backends() {
  std::vector<std::string> output{"cpu"};
#ifdef OPENFLOW_COMPUTE_CUDA
  output.emplace_back("cuda");
#endif
#ifdef OPENFLOW_COMPUTE_ROCM
  output.emplace_back("rocm");
#endif
#ifdef OPENFLOW_COMPUTE_METAL
  output.emplace_back("metal");
#endif
#ifdef OPENFLOW_COMPUTE_VULKAN
  output.emplace_back("vulkan");
#endif
  return output;
}

Value encode_score(const Score& score) {
  Value::Array token_scores;
  for (const double value : score.token_log_probabilities) token_scores.emplace_back(value);
  return Value::Object{{"log_probability", score.log_probability},
                       {"token_count", score.token_count},
                       {"mean_log_probability",
                        score.token_count == 0
                            ? 0.0
                            : score.log_probability / static_cast<double>(score.token_count)},
                       {"token_log_probabilities", std::move(token_scores)}};
}

Value encode_candidate(const EditCandidate& candidate) {
  return Value::Object{{"start_byte", candidate.start_byte},
                       {"end_byte", candidate.end_byte},
                       {"replacement", candidate.replacement},
                       {"kind", edit_kind_name(candidate.kind)}};
}

struct RankedCandidate {
  std::string id;
  std::string text;
  std::size_t input_index{0};
  double log_probability{0.0};
  std::size_t token_count{0};
  double mean_log_probability{0.0};
  double candidate_log_probability{0.0};
  double right_context_log_probability_delta{0.0};
};

struct NormalizationProposal {
  EditCandidate edit;
  NormalizationGrounding grounding{NormalizationGrounding::kLexicalSkeleton};
};

Score score_or_empty(Backend& backend, const std::string& text) {
  return text.empty() ? Score{} : backend.score(text);
}

double continuation_log_probability(const Score& combined, const Score& prefix) {
  return combined.log_probability - prefix.log_probability;
}

Value encode_ranking(const RankedCandidate& candidate) {
  return Value::Object{
      {"id", candidate.id},
      {"log_probability", candidate.log_probability},
      {"token_count", candidate.token_count},
      {"mean_log_probability", candidate.mean_log_probability},
      {"candidate_log_probability", candidate.candidate_log_probability},
      {"right_context_log_probability_delta",
       candidate.right_context_log_probability_delta}};
}

Value encode_normalization(const NormalizationProposal& proposal,
                           const std::string& source) {
  return Value::Object{{"start_byte", proposal.edit.start_byte},
                       {"end_byte", proposal.edit.end_byte},
                       {"source", source},
                       {"replacement", proposal.edit.replacement},
                       {"kind", edit_kind_name(proposal.edit.kind)},
                       {"grounding", normalization_grounding_name(proposal.grounding)}};
}

std::vector<NormalizationProposal> generated_normalizations(
    Backend& backend, const std::string& left_context, const std::string& text,
    const std::string& right_context) {
  const std::string encoded =
      backend.propose_normalizations_json(left_context, text, right_context);
  try {
    const Value document = json::parse(encoded);
    const auto& array = document.as_array();
    if (array.size() > 8) return {};
    std::vector<NormalizationProposal> output;
    for (const auto& item : array) {
      try {
        const std::size_t start = item.at("start_byte").as_size();
        const std::size_t end = item.at("end_byte").as_size();
        const std::string source = item.at("source").as_string();
        const EditCandidate edit{start, end, item.at("replacement").as_string(),
                                 parse_edit_kind(item.at("kind").as_string())};
        const auto grounding =
            parse_normalization_grounding(item.at("grounding").as_string());
        if (start > end || end > text.size() || text.substr(start, end - start) != source) {
          continue;
        }
        if (!validate_normalization_proposal(text, edit, grounding).valid) continue;
        output.push_back(NormalizationProposal{edit, grounding});
      } catch (const std::exception&) {
        // Generated model output is untrusted. A malformed local proposal is
        // ignored and cannot suppress other valid proposals.
      }
    }
    return output;
  } catch (const std::exception&) {
    return {};
  }
}

std::vector<WordEvidence> plain_words(const std::string& text, double confidence) {
  std::vector<WordEvidence> words;
  std::size_t cursor = 0;
  while (cursor < text.size()) {
    while (cursor < text.size() && std::isspace(static_cast<unsigned char>(text[cursor])) != 0) {
      ++cursor;
    }
    if (cursor == text.size()) break;
    const std::size_t start = cursor;
    while (cursor < text.size() && std::isspace(static_cast<unsigned char>(text[cursor])) == 0) {
      ++cursor;
    }
    const std::string word = text.substr(start, cursor - start);
    words.push_back(WordEvidence{word, start, cursor, confidence, looks_protected(word)});
  }
  return words;
}

std::vector<EditCandidate> safe_proposals(const std::string& text) {
  std::vector<EditCandidate> output;
  std::size_t first = 0;
  while (first < text.size() && std::isspace(static_cast<unsigned char>(text[first])) != 0) ++first;
  if (first < text.size() && std::islower(static_cast<unsigned char>(text[first])) != 0) {
    std::string replacement(1, static_cast<char>(std::toupper(static_cast<unsigned char>(text[first]))));
    output.push_back(EditCandidate{first, first + 1, replacement, EditKind::kFormatting});
  }
  for (std::size_t cursor = 0; cursor < text.size();) {
    if (std::isspace(static_cast<unsigned char>(text[cursor])) == 0) {
      ++cursor;
      continue;
    }
    const std::size_t start = cursor;
    while (cursor < text.size() && std::isspace(static_cast<unsigned char>(text[cursor])) != 0) {
      ++cursor;
    }
    if (cursor - start > 1) {
      output.push_back(EditCandidate{start, cursor, " ", EditKind::kFormatting});
    }
  }
  const auto words = plain_words(text, 0.0);
  for (std::size_t index = 1; index < words.size(); ++index) {
    std::string previous = words[index - 1].text;
    std::string current = words[index].text;
    std::transform(previous.begin(), previous.end(), previous.begin(),
                   [](unsigned char value) { return static_cast<char>(std::tolower(value)); });
    std::transform(current.begin(), current.end(), current.begin(),
                   [](unsigned char value) { return static_cast<char>(std::tolower(value)); });
    if (previous == current) {
      output.push_back(EditCandidate{words[index - 1].end_byte, words[index].end_byte, "",
                                     EditKind::kAdjacentDuplicate});
    }
  }
  std::size_t end = text.size();
  while (end > 0 && std::isspace(static_cast<unsigned char>(text[end - 1])) != 0) --end;
  if (end > 0 && text[end - 1] != '.' && text[end - 1] != '!' && text[end - 1] != '?' &&
      static_cast<unsigned char>(text[end - 1]) < 0x80U) {
    output.push_back(EditCandidate{end, end, ".", EditKind::kFormatting});
  }
  return output;
}

std::vector<EditCandidate> generated_proposals(Backend& backend, const std::string& text) {
  const std::string encoded = backend.propose_edits_json(text);
  try {
    const Value document = json::parse(encoded);
    const auto& array = document.as_array();
    if (array.size() > 8) return {};
    std::vector<EditCandidate> output;
    for (const auto& item : array) {
      const std::size_t start = item.at("start_byte").as_size();
      const std::size_t end = item.at("end_byte").as_size();
      const std::string source = item.at("source").as_string();
      const std::string replacement = item.at("replacement").as_string();
      if (start > end || end > text.size() || text.substr(start, end - start) != source) {
        continue;
      }
      if (source.empty() || replacement.empty() || source == replacement) {
        continue;
      }
      output.push_back(EditCandidate{start, end, replacement, EditKind::kLexical});
    }
    return output;
  } catch (const json::Error&) {
    // Generated lexical edits are untrusted optional hints. Discard malformed
    // output so deterministic formatting and duplicate removal still run.
    return {};
  }
}

std::vector<EditCandidate> all_proposals(Backend& backend, const std::string& text) {
  auto output = safe_proposals(text);
  auto generated = generated_proposals(backend, text);
  output.insert(output.end(), std::make_move_iterator(generated.begin()),
                std::make_move_iterator(generated.end()));
  return output;
}

std::vector<EditCandidate> decode_candidates(const Value& params, const std::string& text,
                                             Backend& backend) {
  const auto* encoded = params.find("candidates");
  if (encoded == nullptr) return all_proposals(backend, text);
  if (encoded->as_array().size() > 32) {
    throw std::invalid_argument("cleanup accepts at most 32 candidate edits");
  }
  std::vector<EditCandidate> output;
  for (const auto& candidate : encoded->as_array()) {
    const EditKind kind = parse_edit_kind(json::string_or(candidate, "kind", "lexical"));
    if (kind != EditKind::kFormatting && kind != EditKind::kAdjacentDuplicate &&
        kind != EditKind::kLexical) {
      throw std::invalid_argument("cleanup does not accept normalization proposal kinds");
    }
    output.push_back(EditCandidate{candidate.at("start_byte").as_size(),
                                   candidate.at("end_byte").as_size(),
                                   candidate.at("replacement").as_string(), kind});
  }
  return output;
}

std::vector<WordEvidence> decode_words(const Value& params, const std::string& text) {
  if (const auto* encoded = params.find("words")) {
    std::vector<WordEvidence> output;
    for (const auto& word : encoded->as_array()) {
      const std::string value = word.at("text").as_string();
      const std::size_t start = word.at("start_byte").as_size();
      const std::size_t end = word.at("end_byte").as_size();
      const double confidence = word.at("confidence").as_number();
      if (start > end || end > text.size() || text.substr(start, end - start) != value ||
          !std::isfinite(confidence) || confidence < 0.0 || confidence > 1.0) {
        throw std::invalid_argument("word evidence does not match the transcript");
      }
      output.push_back(WordEvidence{value, start, end, confidence,
                                    json::bool_or(word, "protected", looks_protected(value))});
    }
    return output;
  }
  if (const auto* encoded = params.find("tokens")) {
    std::vector<TokenEvidence> tokens;
    std::string reconstructed;
    for (const auto& token : encoded->as_array()) {
      const std::string value = token.at("text").as_string();
      const double probability = token.at("probability").as_number();
      if (!std::isfinite(probability) || probability < 0.0 || probability > 1.0) {
        throw std::invalid_argument("token probability must be between zero and one");
      }
      reconstructed += value;
      tokens.push_back(TokenEvidence{value, probability});
    }
    if (reconstructed != text) throw std::invalid_argument("token evidence does not reconstruct text");
    return aggregate_word_confidence(tokens);
  }
  return plain_words(text, 1.0);
}

std::vector<ProtectedRange> decode_protected_ranges(const Value& params) {
  std::vector<ProtectedRange> output;
  if (const auto* encoded = params.find("protected_ranges")) {
    for (const auto& range : encoded->as_array()) {
      output.push_back(
          ProtectedRange{range.at("start_byte").as_size(), range.at("end_byte").as_size()});
    }
  }
  return output;
}

CleanupPolicy decode_policy(const Value& params) {
  CleanupPolicy policy;
  const auto* encoded = params.find("policy");
  if (encoded == nullptr) return policy;
  policy.high_confidence = json::number_or(*encoded, "high_confidence", policy.high_confidence);
  policy.low_confidence = json::number_or(*encoded, "low_confidence", policy.low_confidence);
  policy.medium_min_advantage_nats = json::number_or(
      *encoded, "medium_min_advantage_nats", policy.medium_min_advantage_nats);
  policy.low_min_advantage_nats =
      json::number_or(*encoded, "low_min_advantage_nats", policy.low_min_advantage_nats);
  policy.maximum_result_length_change_ratio = json::number_or(
      *encoded, "maximum_result_length_change_ratio", policy.maximum_result_length_change_ratio);
  if (!(policy.low_confidence >= 0.0 && policy.low_confidence < policy.high_confidence &&
        policy.high_confidence <= 1.0 && std::isfinite(policy.medium_min_advantage_nats) &&
        std::isfinite(policy.low_min_advantage_nats) &&
        std::isfinite(policy.maximum_result_length_change_ratio) &&
        policy.maximum_result_length_change_ratio >= 0.0)) {
    throw std::invalid_argument("policy confidence thresholds are invalid");
  }
  return policy;
}

class Service {
 public:
  Value handle(const std::string& command, const Value& params) {
    if (command == "list_backends") {
      return Value::Object{{"backends", string_array(compiled_backends())},
                           {"compute_backends", string_array(compiled_compute_backends())}};
    }
    if (command == "load_model") {
      const std::string path = json::string_or(params, "model_path", "");
      auto next = make_backend(json::string_or(params, "backend", "auto"), path);
      next->load(path);
      backend_ = std::move(next);
      sessions_.clear();
      return Value::Object{{"backend", backend_->name()}, {"model_path", path}};
    }
    if (command == "unload_model") {
      backend_.reset();
      sessions_.clear();
      return Value::Object{{"unloaded", true}};
    }
    if (command == "start_session") {
      require_backend();
      const std::string id = params.at("session_id").as_string();
      if (id.empty()) throw std::invalid_argument("session_id must not be empty");
      if (!sessions_.emplace(id, json::string_or(params, "context", "")).second) {
        throw std::invalid_argument("session already exists");
      }
      return Value::Object{{"session_id", id}};
    }
    if (command == "end_session") {
      const std::string id = params.at("session_id").as_string();
      return Value::Object{{"session_id", id}, {"ended", sessions_.erase(id) != 0}};
    }
    if (command == "score") {
      require_session(params);
      return encode_score(backend_->score(params.at("text").as_string()));
    }
    if (command == "propose_edits") {
      require_session(params);
      Value::Array candidates;
      for (const auto& candidate : all_proposals(*backend_, params.at("text").as_string())) {
        candidates.push_back(encode_candidate(candidate));
      }
      return Value::Object{{"candidates", std::move(candidates)}};
    }
    if (command == "rank_candidates") return rank_candidates(params);
    if (command == "cleanup") return cleanup(params);
    throw std::invalid_argument("unknown LLM command: " + command);
  }

 private:
  void require_backend() const {
    if (backend_ == nullptr) throw std::runtime_error("load_model must be called first");
  }

  void require_session(const Value& params) const {
    require_backend();
    const std::string id = params.at("session_id").as_string();
    if (sessions_.find(id) == sessions_.end()) throw std::invalid_argument("unknown session_id");
  }

  Value rank_candidates(const Value& params) {
    require_session(params);
    const std::string left_context = json::string_or(params, "left_context", "");
    const std::string right_context = json::string_or(params, "right_context", "");
    constexpr std::size_t kMaximumSharedContextBytes = 4096;
    constexpr std::size_t kMaximumCandidateBytes = 1024;
    constexpr std::size_t kMaximumCandidateIdBytes = 128;
    constexpr std::size_t kMaximumCandidates = 16;
    if (left_context.size() + right_context.size() > kMaximumSharedContextBytes) {
      throw std::invalid_argument("rank_candidates shared context exceeds 4096 bytes");
    }
    const auto& encoded_candidates = params.at("candidates").as_array();
    if (encoded_candidates.empty() || encoded_candidates.size() > kMaximumCandidates) {
      throw std::invalid_argument("rank_candidates accepts between 1 and 16 candidates");
    }

    std::vector<RankedCandidate> rankings;
    rankings.reserve(encoded_candidates.size());
    std::unordered_set<std::string> ids;
    const Score left_score = score_or_empty(*backend_, left_context);
    double baseline_right_log_probability = 0.0;
    if (!right_context.empty()) {
      baseline_right_log_probability = continuation_log_probability(
          backend_->score(left_context + right_context), left_score);
    }

    for (std::size_t index = 0; index < encoded_candidates.size(); ++index) {
      const auto& encoded = encoded_candidates[index];
      const std::string id = encoded.at("id").as_string();
      const std::string text = encoded.at("text").as_string();
      if (id.empty() || id.size() > kMaximumCandidateIdBytes || !ids.insert(id).second) {
        throw std::invalid_argument("candidate ids must be unique non-empty strings of at most 128 bytes");
      }
      if (text.empty() || text.size() > kMaximumCandidateBytes ||
          std::all_of(text.begin(), text.end(), [](unsigned char character) {
            return std::isspace(character) != 0;
          })) {
        throw std::invalid_argument("candidate text must contain 1 to 1024 bytes of non-whitespace text");
      }

      const Score candidate_sequence = backend_->score(left_context + text);
      const double candidate_log_probability =
          continuation_log_probability(candidate_sequence, left_score);
      double right_context_delta = 0.0;
      if (!right_context.empty()) {
        const Score full_sequence = backend_->score(left_context + text + right_context);
        const double candidate_right_log_probability =
            continuation_log_probability(full_sequence, candidate_sequence);
        // Contrast against the same right context without this candidate. This
        // keeps common-context likelihood out of the candidate-local score while
        // still measuring how well the wording joins to following speech.
        right_context_delta =
            candidate_right_log_probability - baseline_right_log_probability;
      }
      const std::size_t token_count = backend_->score_token_count(text);
      if (token_count == 0) {
        throw std::invalid_argument("candidate text produced no scoreable tokens");
      }
      const double log_probability = candidate_log_probability + right_context_delta;
      if (!std::isfinite(log_probability)) {
        throw std::runtime_error("backend returned a non-finite candidate score");
      }
      rankings.push_back(RankedCandidate{id,
                                         text,
                                         index,
                                         log_probability,
                                         token_count,
                                         log_probability / static_cast<double>(token_count),
                                         candidate_log_probability,
                                         right_context_delta});
    }

    std::stable_sort(rankings.begin(), rankings.end(), [](const auto& left, const auto& right) {
      if (left.mean_log_probability != right.mean_log_probability) {
        return left.mean_log_probability > right.mean_log_probability;
      }
      if (left.log_probability != right.log_probability) {
        return left.log_probability > right.log_probability;
      }
      return left.input_index < right.input_index;
    });
    Value::Array encoded_rankings;
    for (const auto& ranking : rankings) encoded_rankings.push_back(encode_ranking(ranking));
    Value result = Value::Object{{"rankings", std::move(encoded_rankings)}};

    if (json::bool_or(params, "propose_normalizations", false)) {
      const auto& winner = rankings.front();
      Value::Array proposals;
      for (const auto& proposal : generated_normalizations(
               *backend_, left_context, winner.text, right_context)) {
        proposals.push_back(encode_normalization(
            proposal, winner.text.substr(proposal.edit.start_byte,
                                         proposal.edit.end_byte - proposal.edit.start_byte)));
      }
      result["normalization"] =
          Value::Object{{"candidate_id", winner.id}, {"proposals", std::move(proposals)}};
    }
    return result;
  }

  Value cleanup(const Value& params) {
    require_session(params);
    const std::string text = params.at("text").as_string();
    const auto words = decode_words(params, text);
    const auto protected_ranges = decode_protected_ranges(params);
    const auto policy = decode_policy(params);
    auto candidates = decode_candidates(params, text, *backend_);
    const bool has_lexical = std::any_of(candidates.begin(), candidates.end(), [](const auto& item) {
      return item.kind == EditKind::kLexical;
    });
    // Formatting and exact duplicate gates are deterministic and never consult
    // LLM likelihood. Only lexical proposals pay for model scoring.
    const Score original_score = has_lexical ? backend_->score(text) : Score{};
    std::sort(candidates.begin(), candidates.end(), [](const auto& left, const auto& right) {
      if (left.start_byte != right.start_byte) return left.start_byte < right.start_byte;
      return left.end_byte < right.end_byte;
    });

    Value::Array decisions;
    std::vector<EditCandidate> accepted;
    std::size_t accepted_end = 0;
    bool have_accepted = false;
    for (const auto& candidate : candidates) {
      Score proposed_score;
      std::size_t source_token_count = 0;
      if (candidate.kind == EditKind::kLexical && candidate.start_byte <= candidate.end_byte &&
          candidate.end_byte <= text.size()) {
        const std::string proposed = text.substr(0, candidate.start_byte) + candidate.replacement +
                                     text.substr(candidate.end_byte);
        proposed_score = backend_->score(proposed);
        source_token_count = backend_->score_token_count(
            text.substr(candidate.start_byte, candidate.end_byte - candidate.start_byte));
      }
      EditDecision decision = gate_edit(text, words, protected_ranges, candidate,
                                        original_score.log_probability,
                                        proposed_score.log_probability,
                                        std::max<std::size_t>(1, source_token_count), policy);
      if (decision.accepted && have_accepted && candidate.start_byte < accepted_end) {
        decision.accepted = false;
        decision.reason = "overlapping_edit";
      }
      if (decision.accepted) {
        accepted.push_back(candidate);
        accepted_end = candidate.end_byte;
        have_accepted = true;
      }
      decisions.emplace_back(Value::Object{
          {"edit", encode_candidate(candidate)},
          {"accepted", decision.accepted},
          {"reason", decision.reason},
          {"source_confidence", decision.source_confidence},
          {"llm_advantage_nats_per_token", decision.llm_advantage_nats_per_token},
          {"original_log_probability", original_score.log_probability},
          {"proposed_log_probability", proposed_score.log_probability}});
    }
    return Value::Object{{"text", apply_edits(text, accepted)},
                         {"original_text", text},
                         {"decisions", std::move(decisions)}};
  }

  std::unique_ptr<Backend> backend_;
  std::unordered_map<std::string, std::string> sessions_;
};

}  // namespace
}  // namespace openflow::inference::llm

int main() {
  openflow::inference::llm::Service service;
  return openflow::inference::run_worker(
      "openflow-llm-worker", "0.1.0",
      [&service](const std::string& command, const openflow::inference::json::Value& params) {
        return service.handle(command, params);
      });
}
