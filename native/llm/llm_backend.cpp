#include "llm_backend.hpp"

#include <algorithm>
#include <cctype>
#include <cstdint>
#include <cmath>
#include <limits>
#include <stdexcept>

#ifdef OPENFLOW_HAS_LLAMA_CPP
#include "ggml-backend.h"
#include "llama.h"
#endif

namespace openflow::inference::llm {
namespace {

class MockBackend final : public Backend {
 public:
  std::string name() const override { return "mock"; }
  void load(const std::string&) override {}

  Score score(const std::string& text) override {
    // This is deliberately simple and stable across platforms. It is a CI backend,
    // not a pretend language model: production uses the llama.cpp adapter.
    Score result;
    std::string previous_word;
    bool at_sentence_start = true;
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
      std::string word = text.substr(start, cursor - start);
      std::string normalized = word;
      std::transform(normalized.begin(), normalized.end(), normalized.begin(),
                     [](unsigned char character) { return static_cast<char>(std::tolower(character)); });
      double log_probability = -1.0;
      if (!previous_word.empty() && normalized == previous_word) log_probability -= 2.0;
      if (at_sentence_start && !word.empty() &&
          std::islower(static_cast<unsigned char>(word.front())) != 0) log_probability -= 0.5;
      result.token_log_probabilities.push_back(log_probability);
      result.log_probability += log_probability;
      ++result.token_count;
      previous_word = std::move(normalized);
      at_sentence_start = !word.empty() &&
                          (word.back() == '.' || word.back() == '!' || word.back() == '?');
    }
    if (!text.empty() && text.back() != '.' && text.back() != '!' && text.back() != '?') {
      result.log_probability -= 0.25;
    }
    for (std::size_t index = 1; index < text.size(); ++index) {
      if (text[index] == ' ' && text[index - 1] == ' ') result.log_probability -= 0.5;
    }
    return result;
  }

  std::size_t score_token_count(const std::string& text) override {
    return score(text).token_count;
  }

  std::string propose_edits_json(const std::string&) override { return "[]"; }
};

#ifdef OPENFLOW_HAS_LLAMA_CPP
class LlamaBackend final : public Backend {
 public:
  LlamaBackend() {
    ggml_backend_load_all();
    llama_backend_init();
  }
  ~LlamaBackend() override {
    if (context_ != nullptr) llama_free(context_);
    if (model_ != nullptr) llama_model_free(model_);
    llama_backend_free();
  }

  std::string name() const override { return "llama.cpp"; }

  void load(const std::string& model_path) override {
    if (model_path.empty()) throw std::invalid_argument("llama.cpp requires model_path");
    if (context_ != nullptr) {
      llama_free(context_);
      context_ = nullptr;
    }
    if (model_ != nullptr) {
      llama_model_free(model_);
      model_ = nullptr;
    }
    llama_model_params parameters = llama_model_default_params();
    parameters.n_gpu_layers = 999;
    model_ = llama_model_load_from_file(model_path.c_str(), parameters);
    if (model_ == nullptr) throw std::runtime_error("llama.cpp failed to load model");
    llama_context_params context_parameters = llama_context_default_params();
    context_parameters.n_ctx = kContextTokens;
    context_parameters.n_batch = kBatchTokens;
    context_ = llama_init_from_model(model_, context_parameters);
    if (context_ == nullptr) {
      llama_model_free(model_);
      model_ = nullptr;
      throw std::runtime_error("llama.cpp failed to create reusable inference context");
    }
  }

  Score score(const std::string& text) override {
    require_loaded();
    const llama_vocab* vocabulary = llama_model_get_vocab(model_);
    int token_count = llama_tokenize(vocabulary, text.data(), static_cast<int32_t>(text.size()),
                                     nullptr, 0, true, true);
    if (token_count == 0) return {};
    if (token_count > 0) throw std::runtime_error("llama.cpp token count query was unexpected");
    std::vector<llama_token> tokens(static_cast<std::size_t>(-token_count));
    token_count = llama_tokenize(vocabulary, text.data(), static_cast<int32_t>(text.size()),
                                 tokens.data(), static_cast<int32_t>(tokens.size()), true, true);
    if (token_count < 0) throw std::runtime_error("llama.cpp tokenization failed");
    tokens.resize(static_cast<std::size_t>(token_count));
    if (tokens.size() < 2) return Score{0.0, tokens.size(), {}};
    if (tokens.size() > kContextTokens) {
      throw std::invalid_argument("text exceeds the reusable llama.cpp scoring context");
    }
    llama_memory_clear(llama_get_memory(context_), true);
    llama_batch batch = llama_batch_init(1, 0, 1);
    Score output;
    output.token_count = tokens.size() - 1;
    try {
      const int vocabulary_size = llama_vocab_n_tokens(vocabulary);
      for (std::size_t index = 0; index + 1 < tokens.size(); ++index) {
        batch.n_tokens = 1;
        batch.token[0] = tokens[index];
        batch.pos[0] = static_cast<llama_pos>(index);
        batch.n_seq_id[0] = 1;
        batch.seq_id[0][0] = 0;
        batch.logits[0] = true;
        if (llama_decode(context_, batch) != 0) throw std::runtime_error("llama.cpp decode failed");
        const float* logits = llama_get_logits_ith(context_, 0);
        if (logits == nullptr) throw std::runtime_error("llama.cpp did not return logits");
        float maximum = -std::numeric_limits<float>::infinity();
        for (int token = 0; token < vocabulary_size; ++token) maximum = std::max(maximum, logits[token]);
        double denominator = 0.0;
        for (int token = 0; token < vocabulary_size; ++token) {
          denominator += std::exp(static_cast<double>(logits[token] - maximum));
        }
        const double log_probability =
            static_cast<double>(logits[tokens[index + 1]] - maximum) - std::log(denominator);
        output.token_log_probabilities.push_back(log_probability);
        output.log_probability += log_probability;
      }
    } catch (...) {
      llama_batch_free(batch);
      throw;
    }
    llama_batch_free(batch);
    return output;
  }

  std::size_t score_token_count(const std::string& text) override {
    require_loaded();
    const llama_vocab* vocabulary = llama_model_get_vocab(model_);
    const int count = llama_tokenize(vocabulary, text.data(), static_cast<int32_t>(text.size()),
                                     nullptr, 0, true, true);
    if (count == 0) return 0;
    if (count > 0) throw std::runtime_error("llama.cpp token count query was unexpected");
    return static_cast<std::size_t>(-count - 1);
  }

  std::string propose_edits_json(const std::string& text) override {
    require_loaded();
    static constexpr const char* kGrammar = R"gbnf(
root ::= ws "[" ws (edit (ws "," ws edit)*)? ws "]" ws
edit ::= "{" ws "\"start_byte\"" ws ":" ws integer ws "," ws "\"end_byte\"" ws ":" ws integer ws "," ws "\"source\"" ws ":" ws string ws "," ws "\"replacement\"" ws ":" ws string ws "}"
integer ::= "0" | [1-9] [0-9]*
string ::= "\"" character* "\""
character ::= [^"\\\x00-\x1F] | "\\" (["\\/bfnrt] | "u" [0-9a-fA-F] [0-9a-fA-F] [0-9a-fA-F] [0-9a-fA-F])
ws ::= [ \t\n\r]*
)gbnf";

    const std::string prompt =
        "<|im_start|>system\n"
        "/no_think\nYou conservatively correct speech-recognition word errors. Do not change "
        "punctuation, capitalization, whitespace, numbers, URLs, email, paths, code, or "
        "names. Return at most 8 local lexical edits as JSON only. Offsets are UTF-8 byte "
        "offsets into the exact transcript. Each source must exactly match that range. "
        "Use [] when uncertain. Do not explain or think aloud.\n<|im_end|>\n"
        "<|im_start|>user\n<transcript>" + text +
        "</transcript>\n<|im_end|>\n<|im_start|>assistant\n";

    const llama_vocab* vocabulary = llama_model_get_vocab(model_);
    int prompt_token_count = llama_tokenize(vocabulary, prompt.data(),
                                            static_cast<int32_t>(prompt.size()), nullptr, 0,
                                            true, true);
    if (prompt_token_count >= 0) throw std::runtime_error("llama.cpp token count query failed");
    std::vector<llama_token> prompt_tokens(static_cast<std::size_t>(-prompt_token_count));
    prompt_token_count = llama_tokenize(vocabulary, prompt.data(), static_cast<int32_t>(prompt.size()),
                                        prompt_tokens.data(),
                                        static_cast<int32_t>(prompt_tokens.size()), true, true);
    if (prompt_token_count <= 0) throw std::runtime_error("llama.cpp prompt tokenization failed");
    prompt_tokens.resize(static_cast<std::size_t>(prompt_token_count));

    constexpr std::size_t kMaximumGeneratedTokens = 512;
    if (prompt_tokens.size() > kBatchTokens ||
        prompt_tokens.size() + kMaximumGeneratedTokens + 8 > kContextTokens) {
      throw std::invalid_argument("transcript exceeds the reusable llama.cpp generation context");
    }
    llama_memory_clear(llama_get_memory(context_), true);
    llama_sampler_chain_params sampler_parameters = llama_sampler_chain_default_params();
    llama_sampler* sampler = llama_sampler_chain_init(sampler_parameters);
    if (sampler == nullptr) {
      throw std::runtime_error("llama.cpp failed to create sampler");
    }
    llama_sampler* grammar = llama_sampler_init_grammar(vocabulary, kGrammar, "root");
    if (grammar == nullptr) {
      llama_sampler_free(sampler);
      throw std::runtime_error("llama.cpp failed to parse edit grammar");
    }
    llama_sampler_chain_add(sampler, grammar);
    llama_sampler_chain_add(sampler, llama_sampler_init_greedy());
    llama_batch batch = llama_batch_init(static_cast<int32_t>(prompt_tokens.size()), 0, 1);

    std::string output;
    try {
      batch.n_tokens = static_cast<int32_t>(prompt_tokens.size());
      for (std::size_t index = 0; index < prompt_tokens.size(); ++index) {
        batch.token[index] = prompt_tokens[index];
        batch.pos[index] = static_cast<llama_pos>(index);
        batch.n_seq_id[index] = 1;
        batch.seq_id[index][0] = 0;
        batch.logits[index] = index + 1 == prompt_tokens.size();
      }
      if (llama_decode(context_, batch) != 0) throw std::runtime_error("llama.cpp prompt decode failed");

      int bracket_depth = 0;
      bool in_string = false;
      bool escaped = false;
      bool saw_array = false;
      for (std::size_t generated = 0; generated < kMaximumGeneratedTokens; ++generated) {
        const llama_token token = llama_sampler_sample(sampler, context_, -1);
        if (llama_vocab_is_eog(vocabulary, token)) break;
        // Grammar and chained sampler state advance only when the sampled token
        // is explicitly accepted. Without this, every step is constrained as
        // though it were still the first byte of the JSON document.
        llama_sampler_accept(sampler, token);
        std::vector<char> piece(64);
        int piece_size = llama_token_to_piece(vocabulary, token, piece.data(),
                                              static_cast<int32_t>(piece.size()), 0, true);
        if (piece_size < 0) {
          piece.resize(static_cast<std::size_t>(-piece_size));
          piece_size = llama_token_to_piece(vocabulary, token, piece.data(),
                                            static_cast<int32_t>(piece.size()), 0, true);
        }
        if (piece_size < 0) throw std::runtime_error("llama.cpp token rendering failed");
        const std::string rendered(piece.data(), static_cast<std::size_t>(piece_size));
        output += rendered;
        if (output.size() > 64U * 1024U) throw std::runtime_error("generated edit JSON is too large");
        for (const char character : rendered) {
          if (in_string) {
            if (escaped) escaped = false;
            else if (character == '\\') escaped = true;
            else if (character == '"') in_string = false;
          } else if (character == '"') {
            in_string = true;
          } else if (character == '[') {
            ++bracket_depth;
            saw_array = true;
          } else if (character == ']') {
            --bracket_depth;
          }
        }
        if (saw_array && bracket_depth == 0 && !in_string) break;

        llama_batch next = llama_batch_init(1, 0, 1);
        next.n_tokens = 1;
        next.token[0] = token;
        next.pos[0] = static_cast<llama_pos>(prompt_tokens.size() + generated);
        next.n_seq_id[0] = 1;
        next.seq_id[0][0] = 0;
        next.logits[0] = true;
        const int decode_result = llama_decode(context_, next);
        llama_batch_free(next);
        if (decode_result != 0) throw std::runtime_error("llama.cpp generation decode failed");
      }
    } catch (...) {
      llama_batch_free(batch);
      llama_sampler_free(sampler);
      throw;
    }
    llama_batch_free(batch);
    llama_sampler_free(sampler);
    if (output.empty()) throw std::runtime_error("llama.cpp returned no edit JSON");
    return output;
  }

 private:
  void require_loaded() const {
    if (model_ == nullptr || context_ == nullptr) throw std::runtime_error("model is not loaded");
  }

  static constexpr std::uint32_t kContextTokens = 4096;
  static constexpr std::uint32_t kBatchTokens = 2048;
  llama_model* model_{nullptr};
  llama_context* context_{nullptr};
};
#endif

}  // namespace

std::unique_ptr<Backend> make_backend(const std::string& requested, const std::string& model_path) {
#ifndef OPENFLOW_HAS_LLAMA_CPP
  (void)model_path;
#endif
  const std::string selected = requested.empty() || requested == "auto"
#ifdef OPENFLOW_HAS_LLAMA_CPP
                                   ? (model_path.empty() ? "mock" : "llama.cpp")
#else
                                   ? "mock"
#endif
                                   : requested;
  if (selected == "mock") return std::make_unique<MockBackend>();
#ifdef OPENFLOW_HAS_LLAMA_CPP
  if (selected == "llama.cpp" || selected == "llama") return std::make_unique<LlamaBackend>();
#endif
  throw std::invalid_argument("LLM backend is not compiled in: " + selected);
}

std::vector<std::string> compiled_backends() {
  std::vector<std::string> output{"mock"};
#ifdef OPENFLOW_HAS_LLAMA_CPP
  output.emplace_back("llama.cpp");
#endif
  return output;
}

}  // namespace openflow::inference::llm
