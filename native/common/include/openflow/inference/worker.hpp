#pragma once

#include "openflow/inference/json.hpp"

#include <functional>
#include <string>

namespace openflow::inference {

using CommandHandler =
    std::function<json::Value(const std::string& command, const json::Value& params)>;

// Runs a persistent framed-JSON worker over stdin/stdout. Diagnostics go to stderr.
int run_worker(const std::string& worker_name, const std::string& worker_version,
               const CommandHandler& handler);

}  // namespace openflow::inference
