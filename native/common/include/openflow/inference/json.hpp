#pragma once

#include <cstddef>
#include <map>
#include <stdexcept>
#include <string>
#include <variant>
#include <vector>

namespace openflow::inference::json {

class Error : public std::runtime_error {
 public:
  using std::runtime_error::runtime_error;
};

class Value {
 public:
  using Array = std::vector<Value>;
  using Object = std::map<std::string, Value>;

  Value() = default;
  Value(std::nullptr_t) : value_(nullptr) {}
  Value(bool value) : value_(value) {}
  Value(double value) : value_(value) {}
  Value(int value) : value_(static_cast<double>(value)) {}
  Value(std::size_t value) : value_(static_cast<double>(value)) {}
  Value(const char* value) : value_(std::string(value)) {}
  Value(std::string value) : value_(std::move(value)) {}
  Value(Array value) : value_(std::move(value)) {}
  Value(Object value) : value_(std::move(value)) {}

  [[nodiscard]] bool is_null() const;
  [[nodiscard]] bool is_bool() const;
  [[nodiscard]] bool is_number() const;
  [[nodiscard]] bool is_string() const;
  [[nodiscard]] bool is_array() const;
  [[nodiscard]] bool is_object() const;

  [[nodiscard]] bool as_bool() const;
  [[nodiscard]] double as_number() const;
  [[nodiscard]] std::size_t as_size() const;
  [[nodiscard]] const std::string& as_string() const;
  [[nodiscard]] const Array& as_array() const;
  [[nodiscard]] const Object& as_object() const;
  [[nodiscard]] Array& as_array();
  [[nodiscard]] Object& as_object();

  [[nodiscard]] const Value* find(const std::string& key) const;
  [[nodiscard]] Value* find(const std::string& key);
  [[nodiscard]] const Value& at(const std::string& key) const;
  Value& operator[](const std::string& key);

 private:
  std::variant<std::nullptr_t, bool, double, std::string, Array, Object> value_{nullptr};
};

[[nodiscard]] Value parse(const std::string& input);
[[nodiscard]] std::string dump(const Value& value);

[[nodiscard]] std::string string_or(const Value& object, const std::string& key,
                                    const std::string& fallback);
[[nodiscard]] bool bool_or(const Value& object, const std::string& key, bool fallback);
[[nodiscard]] double number_or(const Value& object, const std::string& key, double fallback);

}  // namespace openflow::inference::json
