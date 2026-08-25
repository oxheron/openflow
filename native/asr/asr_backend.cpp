#include "asr_backend.hpp"

#include <algorithm>
#include <cctype>
#include <cmath>
#include <limits>
#include <stdexcept>
#include <thread>

#ifdef OPENFLOW_HAS_WHISPER_CPP
#include "whisper.h"
#endif

namespace openflow::inference::asr {
namespace {

class MockBackend final : public Backend {
 public:
  std::string name() const override { return "mock"; }
  void load(const std::string&) override {}

  Transcription transcribe(const TranscriptionRequest& request) override {
    Transcription result;
    result.language = request.language == "auto" ? "en" : request.language;
    result.text = request.mock_text;
    Segment segment;
    segment.text = result.text;
    segment.end_ms = static_cast<std::int64_t>(request.samples.size() * 1000U / 16000U);

    std::size_t probability_index = 0;
    std::size_t cursor = 0;
    while (cursor < result.text.size()) {
      const std::size_t start = cursor;
      while (cursor < result.text.size() &&
             std::isspace(static_cast<unsigned char>(result.text[cursor])) != 0) ++cursor;
      while (cursor < result.text.size() &&
             std::isspace(static_cast<unsigned char>(result.text[cursor])) == 0) ++cursor;
      if (cursor == start) break;
      const double probability = probability_index < request.mock_probabilities.size()
                                     ? request.mock_probabilities[probability_index]
                                     : 0.8;
      segment.tokens.push_back(Token{result.text.substr(start, cursor - start),
                                     std::clamp(probability, 0.0, 1.0)});
      ++probability_index;
    }
    if (!result.text.empty()) {
      result.segments.push_back(segment);
      Hypothesis hypothesis;
      hypothesis.text = result.text;
      hypothesis.segments.push_back(std::move(segment));
      double sum_log_probability = 0.0;
      for (const auto& token : hypothesis.segments.front().tokens) {
        hypothesis.tokens.push_back(token);
        sum_log_probability +=
            std::log(std::max(token.probability, std::numeric_limits<double>::min()));
      }
      if (!hypothesis.tokens.empty()) {
        hypothesis.mean_log_probability =
            sum_log_probability / static_cast<double>(hypothesis.tokens.size());
        hypothesis.score = hypothesis.mean_log_probability;
      }
      result.hypotheses.push_back(std::move(hypothesis));
    }
    return result;
  }
};

#ifdef OPENFLOW_HAS_WHISPER_CPP
constexpr std::size_t kMaximumHypotheses = 3;

struct WhisperNBestCollector {
  std::string selected_prefix_text;
  std::vector<Token> selected_prefix_tokens;
  std::vector<Segment> selected_prefix_segments;
  double selected_prefix_sum_log_probability{0.0};
  int selected_prefix_scored_tokens{0};
  std::vector<Hypothesis> hypotheses;
};

void collect_whisper_hypotheses(whisper_context* context, whisper_state*,
                                const whisper_nbest_hypothesis* hypotheses,
                                int hypothesis_count, void* user_data) {
  if (context == nullptr || hypotheses == nullptr || hypothesis_count <= 0 ||
      user_data == nullptr) {
    return;
  }
  auto& collector = *static_cast<WhisperNBestCollector*>(user_data);
  std::vector<Hypothesis> next;
  next.reserve(std::min<std::size_t>(static_cast<std::size_t>(hypothesis_count),
                                     kMaximumHypotheses));

  for (int hypothesis_index = 0;
       hypothesis_index < hypothesis_count && next.size() < kMaximumHypotheses;
       ++hypothesis_index) {
    const auto& source = hypotheses[hypothesis_index];
    Segment window_segment;
    window_segment.start_ms = source.t0_ms;
    window_segment.end_ms = source.t1_ms;

    for (int token_index = 0; token_index < source.n_tokens; ++token_index) {
      const auto& source_token = source.tokens[token_index];
      if (source_token.id >= whisper_token_eot(context)) continue;
      const char* token_text = whisper_token_to_str(context, source_token.id);
      Token token{token_text == nullptr ? "" : token_text,
                  std::clamp(static_cast<double>(source_token.p), 0.0, 1.0)};
      window_segment.text += token.text;
      window_segment.tokens.push_back(std::move(token));
    }

    Hypothesis candidate;
    candidate.text = collector.selected_prefix_text + window_segment.text;
    candidate.score = std::isfinite(source.score) ? source.score : 0.0;
    const int scored_tokens = collector.selected_prefix_scored_tokens + source.n_tokens;
    const double sum_log_probability =
        collector.selected_prefix_sum_log_probability + source.sum_logprobs;
    if (scored_tokens > 0 && std::isfinite(sum_log_probability)) {
      candidate.mean_log_probability =
          sum_log_probability / static_cast<double>(scored_tokens);
    }
    candidate.tokens = collector.selected_prefix_tokens;
    candidate.tokens.insert(candidate.tokens.end(), window_segment.tokens.begin(),
                            window_segment.tokens.end());
    candidate.segments = collector.selected_prefix_segments;
    if (!window_segment.text.empty()) {
      candidate.segments.push_back(std::move(window_segment));
    }

    const bool duplicate =
        std::any_of(next.begin(), next.end(), [&candidate](const Hypothesis& accepted) {
          return accepted.text == candidate.text;
        });
    if (!duplicate) next.push_back(std::move(candidate));
  }

  if (next.empty()) return;
  collector.selected_prefix_text = next.front().text;
  collector.selected_prefix_tokens = next.front().tokens;
  collector.selected_prefix_segments = next.front().segments;
  collector.selected_prefix_sum_log_probability += hypotheses[0].sum_logprobs;
  collector.selected_prefix_scored_tokens += hypotheses[0].n_tokens;
  collector.hypotheses = std::move(next);
}

class WhisperBackend final : public Backend {
 public:
  ~WhisperBackend() override {
    if (context_ != nullptr) whisper_free(context_);
  }

  std::string name() const override { return "whisper.cpp"; }

  void load(const std::string& model_path) override {
    if (model_path.empty()) throw std::invalid_argument("whisper.cpp requires model_path");
    if (context_ != nullptr) {
      whisper_free(context_);
      context_ = nullptr;
    }
    whisper_context_params parameters = whisper_context_default_params();
    context_ = whisper_init_from_file_with_params(model_path.c_str(), parameters);
    if (context_ == nullptr) throw std::runtime_error("whisper.cpp failed to load model");
  }

  Transcription transcribe(const TranscriptionRequest& request) override {
    if (context_ == nullptr) throw std::runtime_error("model is not loaded");
    if (request.samples.empty()) throw std::invalid_argument("samples must not be empty");

    whisper_full_params parameters = whisper_full_default_params(WHISPER_SAMPLING_BEAM_SEARCH);
    parameters.print_progress = false;
    parameters.print_realtime = false;
    parameters.print_timestamps = false;
    parameters.no_context = true;
    parameters.single_segment = !request.final;
    parameters.beam_search.beam_size = 5;
    parameters.n_threads = static_cast<int>(std::max(1U, std::thread::hardware_concurrency()));
    parameters.language = request.language.c_str();
    // In whisper.cpp, a language value of "auto" detects the language and then
    // continues decoding. The detect_language flag is a detection-only mode
    // that returns before transcription, so it must remain disabled here.
    parameters.detect_language = false;
    if (!request.initial_prompt.empty()) parameters.initial_prompt = request.initial_prompt.c_str();
    WhisperNBestCollector nbest_collector;
    parameters.nbest_callback = collect_whisper_hypotheses;
    parameters.nbest_callback_user_data = &nbest_collector;
    if (whisper_full(context_, parameters, request.samples.data(),
                     static_cast<int>(request.samples.size())) != 0) {
      throw std::runtime_error("whisper.cpp transcription failed");
    }

    Transcription output;
    output.language = whisper_lang_str(whisper_full_lang_id(context_));
    const int segment_count = whisper_full_n_segments(context_);
    for (int segment_index = 0; segment_index < segment_count; ++segment_index) {
      Segment segment;
      segment.start_ms = whisper_full_get_segment_t0(context_, segment_index) * 10;
      segment.end_ms = whisper_full_get_segment_t1(context_, segment_index) * 10;
      segment.text = whisper_full_get_segment_text(context_, segment_index);
      output.text += segment.text;
      const int token_count = whisper_full_n_tokens(context_, segment_index);
      for (int token_index = 0; token_index < token_count; ++token_index) {
        const whisper_token token_id =
            whisper_full_get_token_id(context_, segment_index, token_index);
        if (token_id >= whisper_token_eot(context_)) continue;
        const char* text = whisper_full_get_token_text(context_, segment_index, token_index);
        const double probability = whisper_full_get_token_p(context_, segment_index, token_index);
        segment.tokens.push_back(Token{text == nullptr ? "" : text,
                                       std::clamp(probability, 0.0, 1.0)});
      }
      output.segments.push_back(std::move(segment));
    }
    output.hypotheses = std::move(nbest_collector.hypotheses);
    if (!output.hypotheses.empty()) {
      auto primary = std::find_if(output.hypotheses.begin(), output.hypotheses.end(),
                                  [&output](const Hypothesis& hypothesis) {
                                    return hypothesis.text == output.text;
                                  });
      if (primary != output.hypotheses.end() && primary != output.hypotheses.begin()) {
        std::iter_swap(output.hypotheses.begin(), primary);
      }
      if (output.hypotheses.front().text == output.text) {
        output.hypotheses.front().segments = output.segments;
        output.hypotheses.front().tokens.clear();
        for (const auto& segment : output.segments) {
          output.hypotheses.front().tokens.insert(output.hypotheses.front().tokens.end(),
                                                   segment.tokens.begin(),
                                                   segment.tokens.end());
        }
      }
    }
    return output;
  }

 private:
  whisper_context* context_{nullptr};
};
#endif

}  // namespace

std::unique_ptr<Backend> make_backend(const std::string& requested, const std::string& model_path) {
#ifndef OPENFLOW_HAS_WHISPER_CPP
  (void)model_path;
#endif
  const std::string selected = requested.empty() || requested == "auto"
#ifdef OPENFLOW_HAS_WHISPER_CPP
                                   ? (model_path.empty() ? "mock" : "whisper.cpp")
#else
                                   ? "mock"
#endif
                                   : requested;
  if (selected == "mock") return std::make_unique<MockBackend>();
#ifdef OPENFLOW_HAS_WHISPER_CPP
  if (selected == "whisper.cpp" || selected == "whisper") {
    return std::make_unique<WhisperBackend>();
  }
#endif
  throw std::invalid_argument("ASR backend is not compiled in: " + selected);
}

std::vector<std::string> compiled_backends() {
  std::vector<std::string> output{"mock"};
#ifdef OPENFLOW_HAS_WHISPER_CPP
  output.emplace_back("whisper.cpp");
#endif
  return output;
}

}  // namespace openflow::inference::asr
