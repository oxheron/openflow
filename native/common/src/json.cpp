#include "openflow/inference/json.hpp"

#include <cmath>
#include <cstdlib>
#include <iomanip>
#include <limits>
#include <sstream>

namespace openflow::inference::json {
namespace {

template <typename T, typename Variant>
const T& require(const Variant& variant, const char* expected) {
  if (const auto* value = std::get_if<T>(&variant)) return *value;
  throw Error(std::string("expected JSON ") + expected);
}

class Parser {
 public:
  explicit Parser(const std::string& input) : input_(input) {}

  Value parse_document() {
    Value value = parse_value(0);
    skip_whitespace();
    if (position_ != input_.size()) fail("unexpected trailing content");
    return value;
  }

 private:
  static constexpr std::size_t kMaximumDepth = 128;

  [[noreturn]] void fail(const std::string& message) const {
    throw Error(message + " at byte " + std::to_string(position_));
  }

  void skip_whitespace() {
    while (position_ < input_.size()) {
      const char value = input_[position_];
      if (value != ' ' && value != '\n' && value != '\r' && value != '\t') break;
      ++position_;
    }
  }

  bool consume(char expected) {
    if (position_ < input_.size() && input_[position_] == expected) {
      ++position_;
      return true;
    }
    return false;
  }

  void require_literal(const char* literal) {
    for (const char* cursor = literal; *cursor != '\0'; ++cursor) {
      if (position_ >= input_.size() || input_[position_++] != *cursor) {
        fail(std::string("expected ") + literal);
      }
    }
  }

  Value parse_value(std::size_t depth) {
    if (depth > kMaximumDepth) fail("maximum nesting depth exceeded");
    skip_whitespace();
    if (position_ >= input_.size()) fail("expected a value");
    switch (input_[position_]) {
      case 'n':
        require_literal("null");
        return nullptr;
      case 't':
        require_literal("true");
        return true;
      case 'f':
        require_literal("false");
        return false;
      case '"':
        return parse_string();
      case '[':
        return parse_array(depth + 1);
      case '{':
        return parse_object(depth + 1);
      default:
        if (input_[position_] == '-' ||
            (input_[position_] >= '0' && input_[position_] <= '9')) {
          return parse_number();
        }
        fail("unexpected character");
    }
  }

  static void append_utf8(std::string& output, unsigned codepoint) {
    if (codepoint <= 0x7fU) {
      output.push_back(static_cast<char>(codepoint));
    } else if (codepoint <= 0x7ffU) {
      output.push_back(static_cast<char>(0xc0U | (codepoint >> 6U)));
      output.push_back(static_cast<char>(0x80U | (codepoint & 0x3fU)));
    } else if (codepoint <= 0xffffU) {
      output.push_back(static_cast<char>(0xe0U | (codepoint >> 12U)));
      output.push_back(static_cast<char>(0x80U | ((codepoint >> 6U) & 0x3fU)));
      output.push_back(static_cast<char>(0x80U | (codepoint & 0x3fU)));
    } else {
      output.push_back(static_cast<char>(0xf0U | (codepoint >> 18U)));
      output.push_back(static_cast<char>(0x80U | ((codepoint >> 12U) & 0x3fU)));
      output.push_back(static_cast<char>(0x80U | ((codepoint >> 6U) & 0x3fU)));
      output.push_back(static_cast<char>(0x80U | (codepoint & 0x3fU)));
    }
  }

  unsigned parse_hex_quad() {
    unsigned value = 0;
    for (int index = 0; index < 4; ++index) {
      if (position_ >= input_.size()) fail("unterminated unicode escape");
      const char digit = input_[position_++];
      value <<= 4U;
      if (digit >= '0' && digit <= '9') value += static_cast<unsigned>(digit - '0');
      else if (digit >= 'a' && digit <= 'f') value += 10U + static_cast<unsigned>(digit - 'a');
      else if (digit >= 'A' && digit <= 'F') value += 10U + static_cast<unsigned>(digit - 'A');
      else fail("invalid unicode escape");
    }
    return value;
  }

  std::string parse_string() {
    if (!consume('"')) fail("expected string");
    std::string output;
    while (position_ < input_.size()) {
      const unsigned char current = static_cast<unsigned char>(input_[position_++]);
      if (current == '"') return output;
      if (current < 0x20U) fail("control character in string");
      if (current != '\\') {
        output.push_back(static_cast<char>(current));
        continue;
      }
      if (position_ >= input_.size()) fail("unterminated escape");
      switch (input_[position_++]) {
        case '"': output.push_back('"'); break;
        case '\\': output.push_back('\\'); break;
        case '/': output.push_back('/'); break;
        case 'b': output.push_back('\b'); break;
        case 'f': output.push_back('\f'); break;
        case 'n': output.push_back('\n'); break;
        case 'r': output.push_back('\r'); break;
        case 't': output.push_back('\t'); break;
        case 'u': {
          unsigned codepoint = parse_hex_quad();
          if (codepoint >= 0xd800U && codepoint <= 0xdbffU) {
            if (position_ + 2 > input_.size() || input_[position_++] != '\\' ||
                input_[position_++] != 'u') {
              fail("high surrogate without low surrogate");
            }
            const unsigned low = parse_hex_quad();
            if (low < 0xdc00U || low > 0xdfffU) fail("invalid low surrogate");
            codepoint = 0x10000U + ((codepoint - 0xd800U) << 10U) + (low - 0xdc00U);
          } else if (codepoint >= 0xdc00U && codepoint <= 0xdfffU) {
            fail("unexpected low surrogate");
          }
          append_utf8(output, codepoint);
          break;
        }
        default: fail("invalid escape");
      }
    }
    fail("unterminated string");
  }

  Value parse_number() {
    const std::size_t start = position_;
    consume('-');
    if (consume('0')) {
      if (position_ < input_.size() && input_[position_] >= '0' && input_[position_] <= '9') {
        fail("leading zero in number");
      }
    } else {
      if (position_ >= input_.size() || input_[position_] < '1' || input_[position_] > '9') {
        fail("invalid number");
      }
      while (position_ < input_.size() && input_[position_] >= '0' &&
             input_[position_] <= '9') ++position_;
    }
    if (consume('.')) {
      if (position_ >= input_.size() || input_[position_] < '0' || input_[position_] > '9') {
        fail("invalid fraction");
      }
      while (position_ < input_.size() && input_[position_] >= '0' &&
             input_[position_] <= '9') ++position_;
    }
    if (position_ < input_.size() && (input_[position_] == 'e' || input_[position_] == 'E')) {
      ++position_;
      if (position_ < input_.size() && (input_[position_] == '+' || input_[position_] == '-')) {
        ++position_;
      }
      if (position_ >= input_.size() || input_[position_] < '0' || input_[position_] > '9') {
        fail("invalid exponent");
      }
      while (position_ < input_.size() && input_[position_] >= '0' &&
             input_[position_] <= '9') ++position_;
    }
    const std::string encoded = input_.substr(start, position_ - start);
    char* end = nullptr;
    const double value = std::strtod(encoded.c_str(), &end);
    if (end != encoded.c_str() + encoded.size() || !std::isfinite(value)) fail("invalid number");
    return value;
  }

  Value parse_array(std::size_t depth) {
    consume('[');
    Value::Array output;
    skip_whitespace();
    if (consume(']')) return output;
    for (;;) {
      output.push_back(parse_value(depth));
      skip_whitespace();
      if (consume(']')) return output;
      if (!consume(',')) fail("expected ',' or ']'");
    }
  }

  Value parse_object(std::size_t depth) {
    consume('{');
    Value::Object output;
    skip_whitespace();
    if (consume('}')) return output;
    for (;;) {
      skip_whitespace();
      if (position_ >= input_.size() || input_[position_] != '"') fail("expected object key");
      std::string key = parse_string();
      skip_whitespace();
      if (!consume(':')) fail("expected ':'");
      auto [iterator, inserted] = output.emplace(std::move(key), parse_value(depth));
      (void)iterator;
      if (!inserted) fail("duplicate object key");
      skip_whitespace();
      if (consume('}')) return output;
      if (!consume(',')) fail("expected ',' or '}'");
    }
  }

  const std::string& input_;
  std::size_t position_{0};
};

void dump_string(std::ostringstream& output, const std::string& value) {
  output << '"';
  for (const unsigned char character : value) {
    switch (character) {
      case '"': output << "\\\""; break;
      case '\\': output << "\\\\"; break;
      case '\b': output << "\\b"; break;
      case '\f': output << "\\f"; break;
      case '\n': output << "\\n"; break;
      case '\r': output << "\\r"; break;
      case '\t': output << "\\t"; break;
      default:
        if (character < 0x20U) {
          output << "\\u" << std::hex << std::setw(4) << std::setfill('0')
                 << static_cast<unsigned>(character) << std::dec << std::setfill(' ');
        } else {
          output << static_cast<char>(character);
        }
    }
  }
  output << '"';
}

void dump_value(std::ostringstream& output, const Value& value) {
  if (value.is_null()) output << "null";
  else if (value.is_bool()) output << (value.as_bool() ? "true" : "false");
  else if (value.is_number()) output << std::setprecision(17) << value.as_number();
  else if (value.is_string()) dump_string(output, value.as_string());
  else if (value.is_array()) {
    output << '[';
    bool first = true;
    for (const auto& child : value.as_array()) {
      if (!first) output << ',';
      first = false;
      dump_value(output, child);
    }
    output << ']';
  } else {
    output << '{';
    bool first = true;
    for (const auto& [key, child] : value.as_object()) {
      if (!first) output << ',';
      first = false;
      dump_string(output, key);
      output << ':';
      dump_value(output, child);
    }
    output << '}';
  }
}

}  // namespace

bool Value::is_null() const { return std::holds_alternative<std::nullptr_t>(value_); }
bool Value::is_bool() const { return std::holds_alternative<bool>(value_); }
bool Value::is_number() const { return std::holds_alternative<double>(value_); }
bool Value::is_string() const { return std::holds_alternative<std::string>(value_); }
bool Value::is_array() const { return std::holds_alternative<Array>(value_); }
bool Value::is_object() const { return std::holds_alternative<Object>(value_); }
bool Value::as_bool() const { return require<bool>(value_, "boolean"); }
double Value::as_number() const { return require<double>(value_, "number"); }
std::size_t Value::as_size() const {
  const double value = as_number();
  if (value < 0 || value > static_cast<double>(std::numeric_limits<std::size_t>::max()) ||
      std::floor(value) != value) throw Error("expected non-negative integer");
  return static_cast<std::size_t>(value);
}
const std::string& Value::as_string() const { return require<std::string>(value_, "string"); }
const Value::Array& Value::as_array() const { return require<Array>(value_, "array"); }
const Value::Object& Value::as_object() const { return require<Object>(value_, "object"); }
Value::Array& Value::as_array() { return const_cast<Array&>(require<Array>(value_, "array")); }
Value::Object& Value::as_object() { return const_cast<Object&>(require<Object>(value_, "object")); }
const Value* Value::find(const std::string& key) const {
  const auto& object = as_object();
  const auto iterator = object.find(key);
  return iterator == object.end() ? nullptr : &iterator->second;
}
Value* Value::find(const std::string& key) {
  auto& object = as_object();
  const auto iterator = object.find(key);
  return iterator == object.end() ? nullptr : &iterator->second;
}
const Value& Value::at(const std::string& key) const {
  const auto* value = find(key);
  if (value == nullptr) throw Error("missing required field '" + key + "'");
  return *value;
}
Value& Value::operator[](const std::string& key) {
  if (is_null()) value_ = Object{};
  return as_object()[key];
}

Value parse(const std::string& input) { return Parser(input).parse_document(); }
std::string dump(const Value& value) {
  std::ostringstream output;
  dump_value(output, value);
  return output.str();
}
std::string string_or(const Value& object, const std::string& key, const std::string& fallback) {
  const auto* value = object.find(key);
  return value == nullptr ? fallback : value->as_string();
}
bool bool_or(const Value& object, const std::string& key, bool fallback) {
  const auto* value = object.find(key);
  return value == nullptr ? fallback : value->as_bool();
}
double number_or(const Value& object, const std::string& key, double fallback) {
  const auto* value = object.find(key);
  return value == nullptr ? fallback : value->as_number();
}

}  // namespace openflow::inference::json
