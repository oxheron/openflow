#include "openflow/inference/framing.hpp"
#include "openflow/inference/json.hpp"

#include <iostream>
#include <sstream>
#include <string>

namespace {

int failures = 0;

void expect(bool condition, const std::string& message) {
  if (!condition) {
    std::cerr << "FAILED: " << message << '\n';
    ++failures;
  }
}

}  // namespace

int main() {
  using namespace openflow::inference;
  const auto parsed = json::parse(
      R"({"command":"ping","params":{"enabled":true,"items":[1,null,"\u00e9"]}})");
  expect(parsed.at("command").as_string() == "ping", "object string field parses");
  expect(parsed.at("params").at("items").as_array()[2].as_string() == "é",
         "unicode escape becomes UTF-8");
  expect(json::parse(json::dump(parsed)).at("params").at("enabled").as_bool(),
         "serialized JSON parses again");

  std::stringstream stream(std::ios::in | std::ios::out | std::ios::binary);
  std::string error;
  expect(write_frame(stream, json::dump(parsed), error), "frame writes");
  stream.seekg(0);
  std::string payload;
  expect(read_frame(stream, payload, error) == ReadStatus::kOk, "frame reads");
  expect(json::parse(payload).at("command").as_string() == "ping", "frame payload is intact");

  bool rejected_duplicate = false;
  try {
    (void)json::parse(R"({"x":1,"x":2})");
  } catch (const json::Error&) {
    rejected_duplicate = true;
  }
  expect(rejected_duplicate, "duplicate object keys are rejected");

  if (failures != 0) std::cerr << failures << " JSON/framing test(s) failed\n";
  return failures == 0 ? 0 : 1;
}
