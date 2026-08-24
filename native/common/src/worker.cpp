#include "openflow/inference/worker.hpp"

#include "openflow/inference/framing.hpp"

#include <exception>
#include <iostream>

namespace openflow::inference {
namespace {

json::Value error_response(const json::Value& id, const std::string& code,
                           const std::string& message) {
  return json::Value::Object{
      {"id", id},
      {"ok", false},
      {"error", json::Value::Object{{"code", code}, {"message", message}}}};
}

}  // namespace

int run_worker(const std::string& worker_name, const std::string& worker_version,
               const CommandHandler& handler) {
  for (;;) {
    std::string encoded;
    std::string io_error;
    const ReadStatus status = read_frame(std::cin, encoded, io_error);
    if (status == ReadStatus::kEndOfStream) return 0;
    if (status == ReadStatus::kError) {
      std::cerr << worker_name << ": protocol error: " << io_error << '\n';
      return 2;
    }

    json::Value id = nullptr;
    json::Value response;
    bool should_shutdown = false;
    try {
      const json::Value request = json::parse(encoded);
      if (!request.is_object()) throw json::Error("request must be an object");
      if (const auto* request_id = request.find("id")) id = *request_id;
      const std::string command = request.at("command").as_string();
      const json::Value empty_params = json::Value::Object{};
      const json::Value* params = request.find("params");
      if (params == nullptr) params = &empty_params;
      if (!params->is_object()) throw json::Error("params must be an object");

      json::Value result;
      if (command == "ping") {
        result = json::Value::Object{{"worker", worker_name},
                                     {"version", worker_version},
                                     {"protocol_version", 1}};
      } else if (command == "shutdown") {
        result = json::Value::Object{{"accepted", true}};
        should_shutdown = true;
      } else {
        result = handler(command, *params);
      }
      response = json::Value::Object{{"id", id}, {"ok", true}, {"result", result}};
    } catch (const json::Error& error) {
      response = error_response(id, "invalid_request", error.what());
    } catch (const std::invalid_argument& error) {
      response = error_response(id, "invalid_request", error.what());
    } catch (const std::exception& error) {
      response = error_response(id, "worker_error", error.what());
    }

    if (!write_frame(std::cout, json::dump(response), io_error)) {
      std::cerr << worker_name << ": " << io_error << '\n';
      return 3;
    }
    if (should_shutdown) return 0;
  }
}

}  // namespace openflow::inference
