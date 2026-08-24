#include "asr_backend.hpp"

#include "openflow/inference/json.hpp"
#include "openflow/inference/worker.hpp"

#include <cmath>
#include <cstdint>
#include <memory>
#include <stdexcept>
#include <string>
#include <unordered_map>
#include <vector>

namespace openflow::inference::asr {
namespace {

using json::Value;

int base64_value(unsigned char character) {
  if (character >= 'A' && character <= 'Z') return character - 'A';
  if (character >= 'a' && character <= 'z') return character - 'a' + 26;
  if (character >= '0' && character <= '9') return character - '0' + 52;
  if (character == '+') return 62;
  if (character == '/') return 63;
  return -1;
}

std::vector<std::uint8_t> decode_base64(const std::string& encoded) {
  if (encoded.size() % 4 != 0) throw std::invalid_argument("base64 PCM has invalid length");
  std::vector<std::uint8_t> output;
  output.reserve(encoded.size() / 4 * 3);
  for (std::size_t offset = 0; offset < encoded.size(); offset += 4) {
    const bool final_block = offset + 4 == encoded.size();
    const int first = base64_value(static_cast<unsigned char>(encoded[offset]));
    const int second = base64_value(static_cast<unsigned char>(encoded[offset + 1]));
    const bool third_padding = encoded[offset + 2] == '=';
    const bool fourth_padding = encoded[offset + 3] == '=';
    const int third = third_padding
                          ? 0
                          : base64_value(static_cast<unsigned char>(encoded[offset + 2]));
    const int fourth = fourth_padding
                           ? 0
                           : base64_value(static_cast<unsigned char>(encoded[offset + 3]));
    if (first < 0 || second < 0 || third < 0 || fourth < 0 ||
        (third_padding && !fourth_padding) || ((third_padding || fourth_padding) && !final_block) ||
        (third_padding && (second & 0x0f) != 0) || (fourth_padding && (third & 0x03) != 0)) {
      throw std::invalid_argument("base64 PCM is not canonical standard base64");
    }
    output.push_back(static_cast<std::uint8_t>((first << 2) | (second >> 4)));
    if (!third_padding) {
      output.push_back(static_cast<std::uint8_t>((second << 4) | (third >> 2)));
    }
    if (!fourth_padding) {
      output.push_back(static_cast<std::uint8_t>((third << 6) | fourth));
    }
  }
  return output;
}

struct Session {
  std::string language{"auto"};
  std::string initial_prompt;
};

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

Value transcription_json(const Transcription& transcription, const std::string& session_id,
                         bool final) {
  Value::Array segments;
  Value::Array flat_tokens;
  for (const auto& segment : transcription.segments) {
    Value::Array tokens;
    for (const auto& token : segment.tokens) {
      Value encoded = Value::Object{{"text", token.text}, {"probability", token.probability}};
      tokens.push_back(encoded);
      flat_tokens.push_back(std::move(encoded));
    }
    segments.emplace_back(Value::Object{{"start_ms", static_cast<double>(segment.start_ms)},
                                        {"end_ms", static_cast<double>(segment.end_ms)},
                                        {"text", segment.text},
                                        {"tokens", std::move(tokens)}});
  }
  return Value::Object{{"session_id", session_id},
                       {"final", final},
                       {"text", transcription.text},
                       {"language", transcription.language},
                       {"tokens", std::move(flat_tokens)},
                       {"segments", std::move(segments)}};
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
      const std::string session_id = params.at("session_id").as_string();
      if (session_id.empty()) throw std::invalid_argument("session_id must not be empty");
      Session session{json::string_or(params, "language", "auto"),
                      json::string_or(params, "initial_prompt", "")};
      if (!sessions_.emplace(session_id, std::move(session)).second) {
        throw std::invalid_argument("session already exists");
      }
      return Value::Object{{"session_id", session_id}};
    }
    if (command == "end_session") {
      const std::string session_id = params.at("session_id").as_string();
      return Value::Object{{"session_id", session_id},
                           {"ended", sessions_.erase(session_id) != 0}};
    }
    if (command == "transcribe") return transcribe(params);
    throw std::invalid_argument("unknown ASR command: " + command);
  }

 private:
  void require_backend() const {
    if (backend_ == nullptr) throw std::runtime_error("load_model must be called first");
  }

  Value transcribe(const Value& params) {
    require_backend();
    const std::string session_id = params.at("session_id").as_string();
    const auto iterator = sessions_.find(session_id);
    if (iterator == sessions_.end()) throw std::invalid_argument("unknown session_id");
    TranscriptionRequest request;
    request.language = iterator->second.language;
    request.initial_prompt = iterator->second.initial_prompt;
    request.final = json::bool_or(params, "final", false);
    request.mock_text = json::string_or(params, "mock_text", "");
    if (params.find("samples") != nullptr && params.find("samples_s16le_base64") != nullptr) {
      throw std::invalid_argument("provide samples or samples_s16le_base64, not both");
    }
    if (const auto* encoded = params.find("samples_s16le_base64")) {
      const auto bytes = decode_base64(encoded->as_string());
      if (bytes.size() % 2 != 0) {
        throw std::invalid_argument("base64 PCM must contain complete S16LE samples");
      }
      request.samples.reserve(bytes.size() / 2);
      for (std::size_t offset = 0; offset < bytes.size(); offset += 2) {
        int sample = static_cast<int>(bytes[offset]) |
                     (static_cast<int>(bytes[offset + 1]) << 8);
        if (sample >= 32768) sample -= 65536;
        request.samples.push_back(static_cast<float>(sample) / 32768.0F);
      }
    }
    if (const auto* samples = params.find("samples")) {
      for (const auto& value : samples->as_array()) {
        const double sample = value.as_number();
        if (!std::isfinite(sample) || sample < -1.0 || sample > 1.0) {
          throw std::invalid_argument("samples must be finite normalized f32 values");
        }
        request.samples.push_back(static_cast<float>(sample));
      }
    }
    if (const auto* probabilities = params.find("mock_probabilities")) {
      for (const auto& value : probabilities->as_array()) {
        const double probability = value.as_number();
        if (!std::isfinite(probability) || probability < 0.0 || probability > 1.0) {
          throw std::invalid_argument("mock probabilities must be between zero and one");
        }
        request.mock_probabilities.push_back(probability);
      }
    }
    return transcription_json(backend_->transcribe(request), session_id, request.final);
  }

  std::unique_ptr<Backend> backend_;
  std::unordered_map<std::string, Session> sessions_;
};

}  // namespace
}  // namespace openflow::inference::asr

int main() {
  openflow::inference::asr::Service service;
  return openflow::inference::run_worker(
      "openflow-asr-worker", "0.1.0",
      [&service](const std::string& command, const openflow::inference::json::Value& params) {
        return service.handle(command, params);
      });
}
