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

// Standalone C++ regression: compile against both Windows STL exception modes.
#include "livekit/bridge_error.h"

#include <cassert>
#include <cstring>
#include <string>
#include <utility>

int main() {
  using livekit_ffi::BridgeError;
  std::string source(128, 'a');
  const BridgeError copied(source);
  assert(copied.what() != source.data());
  source[0] = 'z';
  assert(copied.what()[0] == 'a');

  // The copy must retain its own storage after the original is destroyed.
  const BridgeError retained = [] {
    const BridgeError original(std::string(128, 'b'));
    return BridgeError(original);
  }();
  assert(std::strcmp(retained.what(), std::string(128, 'b').c_str()) == 0);
  BridgeError movable(std::string(128, 'c'));
  const BridgeError moved(std::move(movable));
  assert(std::strcmp(moved.what(), std::string(128, 'c').c_str()) == 0);

  // Match CXX: catch only std::exception, then copy what() after throw's
  // temporary source and the originating stack have been destroyed.
  for (int i = 0; i < 4096; ++i) {
    const auto expected = std::string("Failed to decrypt data packet: ") +
                          std::string(128, 'x') + std::to_string(i);
    try {
      throw BridgeError(std::string(expected));
    } catch (const std::exception& error) {
      const std::string churn(expected.size(), 'y');
      const std::string received(error.what());
      assert(received == expected);
      assert(churn != received);
    }
  }
}
