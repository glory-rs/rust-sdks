/*
 * Copyright 2025 LiveKit, Inc.
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 *     http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

#ifndef LIVEKIT_BRIDGE_ERROR_H_
#define LIVEKIT_BRIDGE_ERROR_H_

#include <exception>
#include <string>
#include <utility>

namespace livekit_ffi {

// The Windows WebRTC archive exports _HAS_EXCEPTIONS=0. With this setting,
// MSVC's std::runtime_error only borrows its constructor's message, even when
// the bridge is compiled with /EHsc. An error built from a temporary string
// therefore leaves CXX's catch/what() with a dangling pointer. Own the message
// without changing the archive's STL configuration or ABI.
class BridgeError final : public std::exception {
 public:
  explicit BridgeError(std::string message) : message_(std::move(message)) {}

  const char* what() const noexcept override { return message_.c_str(); }

 private:
  std::string message_;
};

}  // namespace livekit_ffi

#endif  // LIVEKIT_BRIDGE_ERROR_H_
