#pragma once

#include <cstddef>
#include <cstdint>
#include <istream>
#include <ostream>
#include <string>

namespace openflow::inference {

constexpr std::size_t kMaximumFrameBytes = 16U * 1024U * 1024U;

enum class ReadStatus { kOk, kEndOfStream, kError };

ReadStatus read_frame(std::istream& input, std::string& payload, std::string& error,
                      std::size_t maximum_bytes = kMaximumFrameBytes);
bool write_frame(std::ostream& output, const std::string& payload, std::string& error);

}  // namespace openflow::inference
