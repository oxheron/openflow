#include "openflow/inference/framing.hpp"

#include <array>
#include <limits>

namespace openflow::inference {

ReadStatus read_frame(std::istream& input, std::string& payload, std::string& error,
                      std::size_t maximum_bytes) {
  payload.clear();
  error.clear();
  std::array<unsigned char, 4> header{};
  input.read(reinterpret_cast<char*>(header.data()), static_cast<std::streamsize>(header.size()));
  if (input.gcount() == 0 && input.eof()) return ReadStatus::kEndOfStream;
  if (input.gcount() != static_cast<std::streamsize>(header.size())) {
    error = "truncated frame header";
    return ReadStatus::kError;
  }
  const std::uint32_t length = (static_cast<std::uint32_t>(header[0]) << 24U) |
                               (static_cast<std::uint32_t>(header[1]) << 16U) |
                               (static_cast<std::uint32_t>(header[2]) << 8U) |
                               static_cast<std::uint32_t>(header[3]);
  if (length == 0U) {
    error = "empty frames are not valid JSON requests";
    return ReadStatus::kError;
  }
  if (length > maximum_bytes) {
    error = "frame exceeds configured maximum";
    return ReadStatus::kError;
  }
  payload.resize(length);
  input.read(payload.data(), static_cast<std::streamsize>(length));
  if (input.gcount() != static_cast<std::streamsize>(length)) {
    error = "truncated frame payload";
    payload.clear();
    return ReadStatus::kError;
  }
  return ReadStatus::kOk;
}

bool write_frame(std::ostream& output, const std::string& payload, std::string& error) {
  error.clear();
  if (payload.empty() || payload.size() > std::numeric_limits<std::uint32_t>::max()) {
    error = "payload size cannot be represented by the framing protocol";
    return false;
  }
  const auto length = static_cast<std::uint32_t>(payload.size());
  const std::array<unsigned char, 4> header{
      static_cast<unsigned char>((length >> 24U) & 0xffU),
      static_cast<unsigned char>((length >> 16U) & 0xffU),
      static_cast<unsigned char>((length >> 8U) & 0xffU),
      static_cast<unsigned char>(length & 0xffU)};
  output.write(reinterpret_cast<const char*>(header.data()),
               static_cast<std::streamsize>(header.size()));
  output.write(payload.data(), static_cast<std::streamsize>(payload.size()));
  output.flush();
  if (!output.good()) {
    error = "failed to write response frame";
    return false;
  }
  return true;
}

}  // namespace openflow::inference
