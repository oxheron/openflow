#include "asr_backend.hpp"

#include <algorithm>
#include <cctype>
#include <cmath>
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
    if (!result.text.empty()) result.segments.push_back(std::move(segment));
    return result;
  }
};

#ifdef OPENFLOW_HAS_WHISPER_CPP
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
    parameters.n_threads = static_cast<int>(std::max(1U, std::thread::hardware_concurrency()));
    parameters.language = request.language.c_str();
    // In whisper.cpp, a language value of "auto" detects the language and then
    // continues decoding. The detect_language flag is a detection-only mode
    // that returns before transcription, so it must remain disabled here.
    parameters.detect_language = false;
    if (!request.initial_prompt.empty()) parameters.initial_prompt = request.initial_prompt.c_str();
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
