#pragma once

#include <cstddef>
#include <memory>
#include <string>
#include <vector>

namespace openflow::inference::llm {

struct Score {
  double log_probability{0.0};
  std::size_t token_count{0};
  std::vector<double> token_log_probabilities;
};

class Backend {
 public:
  virtual ~Backend() = default;
  virtual std::string name() const = 0;
  virtual void load(const std::string& model_path) = 0;
  virtual Score score(const std::string& text) = 0;
  virtual std::size_t score_token_count(const std::string& text) = 0;
  // Returns a JSON array of lexical edit objects. Implementations must not
  // classify generated changes as trusted formatting edits.
  virtual std::string propose_edits_json(const std::string& text) = 0;
  // Returns only exact-span, structured surface-normalization proposals. The
  // service validates and exposes these as untrusted suggestions; it never
  // accepts a generated replacement transcript.
  virtual std::string propose_normalizations_json(const std::string& left_context,
                                                  const std::string& text,
                                                  const std::string& right_context) = 0;
};

std::unique_ptr<Backend> make_backend(const std::string& requested, const std::string& model_path);
std::vector<std::string> compiled_backends();

}  // namespace openflow::inference::llm
