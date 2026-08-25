cmake_minimum_required(VERSION 3.20)

project(openflow_native_inference VERSION 0.1.0 LANGUAGES CXX)

option(OPENFLOW_BUILD_TESTS "Build native inference unit tests" ON)
option(OPENFLOW_ENABLE_WHISPER_CPP "Build the whisper.cpp ASR adapter" OFF)
option(OPENFLOW_ENABLE_LLAMA_CPP "Build the llama.cpp scoring adapter" OFF)
set(OPENFLOW_WHISPER_CPP_DIR "" CACHE PATH "Path to a whisper.cpp checkout")
set(OPENFLOW_LLAMA_CPP_DIR "" CACHE PATH "Path to a llama.cpp checkout")

if(OPENFLOW_ENABLE_WHISPER_CPP AND OPENFLOW_ENABLE_LLAMA_CPP)
  message(FATAL_ERROR
    "Build the Whisper and llama adapters in separate CMake directories; "
    "their pinned upstreams carry incompatible ggml revisions.")
endif()

function(openflow_target_defaults target)
  target_compile_features(${target} PUBLIC cxx_std_17)
  if(MSVC)
    target_compile_options(${target} PRIVATE /W4 /permissive-)
  else()
    target_compile_options(${target} PRIVATE -Wall -Wextra -Wpedantic)
  endif()
endfunction()

add_library(openflow-inference-common STATIC
  common/src/confidence.cpp
  common/src/framing.cpp
  common/src/json.cpp
  common/src/worker.cpp
)
target_include_directories(openflow-inference-common PUBLIC common/include)
openflow_target_defaults(openflow-inference-common)

add_executable(openflow-asr-worker
  asr/asr_backend.cpp
  asr/main.cpp
)
target_link_libraries(openflow-asr-worker PRIVATE openflow-inference-common)
target_include_directories(openflow-asr-worker PRIVATE asr)
openflow_target_defaults(openflow-asr-worker)

if(OPENFLOW_ENABLE_WHISPER_CPP)
  if(NOT OPENFLOW_WHISPER_CPP_DIR OR NOT EXISTS "${OPENFLOW_WHISPER_CPP_DIR}/CMakeLists.txt")
    message(FATAL_ERROR "OPENFLOW_ENABLE_WHISPER_CPP requires OPENFLOW_WHISPER_CPP_DIR")
  endif()
  file(READ "${OPENFLOW_WHISPER_CPP_DIR}/include/whisper.h" OPENFLOW_WHISPER_HEADER)
  string(FIND "${OPENFLOW_WHISPER_HEADER}" "whisper_nbest_callback" OPENFLOW_WHISPER_NBEST_API)
  if(OPENFLOW_WHISPER_NBEST_API EQUAL -1)
    message(FATAL_ERROR
      "The whisper.cpp checkout is missing OpenFlow's reviewed N-best patch. "
      "Apply packaging/patches/whisper.cpp-v1.9.1-nbest.patch to the pinned v1.9.1 source.")
  endif()
  set(WHISPER_BUILD_TESTS OFF CACHE BOOL "" FORCE)
  set(WHISPER_BUILD_EXAMPLES OFF CACHE BOOL "" FORCE)
  set(WHISPER_BUILD_SERVER OFF CACHE BOOL "" FORCE)
  add_subdirectory("${OPENFLOW_WHISPER_CPP_DIR}" "${CMAKE_BINARY_DIR}/whisper.cpp" EXCLUDE_FROM_ALL)
  target_compile_definitions(openflow-asr-worker PRIVATE OPENFLOW_HAS_WHISPER_CPP=1)
  target_link_libraries(openflow-asr-worker PRIVATE whisper)
endif()

add_executable(openflow-llm-worker
  llm/llm_backend.cpp
  llm/main.cpp
)
target_link_libraries(openflow-llm-worker PRIVATE openflow-inference-common)
target_include_directories(openflow-llm-worker PRIVATE llm)
openflow_target_defaults(openflow-llm-worker)

foreach(worker_target openflow-asr-worker openflow-llm-worker)
  if(GGML_CUDA)
    target_compile_definitions(${worker_target} PRIVATE OPENFLOW_COMPUTE_CUDA=1)
  endif()
  if(GGML_HIP)
    target_compile_definitions(${worker_target} PRIVATE OPENFLOW_COMPUTE_ROCM=1)
  endif()
  if(GGML_METAL)
    target_compile_definitions(${worker_target} PRIVATE OPENFLOW_COMPUTE_METAL=1)
  endif()
  if(GGML_VULKAN)
    target_compile_definitions(${worker_target} PRIVATE OPENFLOW_COMPUTE_VULKAN=1)
  endif()
endforeach()

if(OPENFLOW_ENABLE_LLAMA_CPP)
  if(NOT OPENFLOW_LLAMA_CPP_DIR OR NOT EXISTS "${OPENFLOW_LLAMA_CPP_DIR}/CMakeLists.txt")
    message(FATAL_ERROR "OPENFLOW_ENABLE_LLAMA_CPP requires OPENFLOW_LLAMA_CPP_DIR")
  endif()
  set(LLAMA_BUILD_TESTS OFF CACHE BOOL "" FORCE)
  set(LLAMA_BUILD_EXAMPLES OFF CACHE BOOL "" FORCE)
  set(LLAMA_BUILD_SERVER OFF CACHE BOOL "" FORCE)
  add_subdirectory("${OPENFLOW_LLAMA_CPP_DIR}" "${CMAKE_BINARY_DIR}/llama.cpp" EXCLUDE_FROM_ALL)
  target_compile_definitions(openflow-llm-worker PRIVATE OPENFLOW_HAS_LLAMA_CPP=1)
  target_link_libraries(openflow-llm-worker PRIVATE llama)
endif()

if(OPENFLOW_BUILD_TESTS)
  include(CTest)
  find_package(Python3 COMPONENTS Interpreter QUIET)
  add_executable(openflow-confidence-tests tests/confidence_test.cpp)
  target_link_libraries(openflow-confidence-tests PRIVATE openflow-inference-common)
  openflow_target_defaults(openflow-confidence-tests)
  add_test(NAME openflow-confidence-tests COMMAND openflow-confidence-tests)

  add_executable(openflow-json-tests tests/json_test.cpp)
  target_link_libraries(openflow-json-tests PRIVATE openflow-inference-common)
  openflow_target_defaults(openflow-json-tests)
  add_test(NAME openflow-json-tests COMMAND openflow-json-tests)

  if(Python3_Interpreter_FOUND)
    add_test(
      NAME openflow-worker-protocol-tests
      COMMAND "${Python3_EXECUTABLE}" "${CMAKE_CURRENT_SOURCE_DIR}/tests/worker_protocol_test.py"
              "$<TARGET_FILE:openflow-asr-worker>" "$<TARGET_FILE:openflow-llm-worker>"
    )
  else()
    message(WARNING "Python 3 was not found; framed worker integration tests are disabled")
  endif()
endif()
