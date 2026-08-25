#pragma once

#include <cstdint>
#include <memory>
#include <string>
#include <vector>

namespace openflow::inference::asr {

struct TranscriptionRequest {
  std::vector<float> samples;
  std::string language{"auto"};
  std::string initial_prompt;
  bool final{false};
  std::string mock_text;
  std::vector<double> mock_probabilities;
};

struct Token {
  std::string text;
  double probability{0.0};
};

struct Segment {
  std::int64_t start_ms{0};
  std::int64_t end_ms{0};
  std::string text;
  std::vector<Token> tokens;
};

struct Hypothesis {
  std::string text;
  double score{0.0};
  double mean_log_probability{0.0};
  std::vector<Token> tokens;
  std::vector<Segment> segments;
};

struct Transcription {
  std::string text;
  std::string language;
  std::vector<Segment> segments;
  std::vector<Hypothesis> hypotheses;
};

class Backend {
 public:
  virtual ~Backend() = default;
  virtual std::string name() const = 0;
  virtual void load(const std::string& model_path) = 0;
  virtual Transcription transcribe(const TranscriptionRequest& request) = 0;
};

std::unique_ptr<Backend> make_backend(const std::string& requested, const std::string& model_path);
std::vector<std::string> compiled_backends();

}  // namespace openflow::inference::asr
