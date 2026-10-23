# CXX error ownership regression

`bridge_error.cpp` deliberately checks message ownership while the source is
still alive, then copy/move and throw/catch lifetimes with heap churn. It needs
no WebRTC archive, CXX build, network, or media device. From an x64 MSVC developer
shell, compile and run it twice, with `_HAS_EXCEPTIONS=0` and `=1`, `/MT /EHsc
/std:c++20`, and `webrtc-sys/include` on the include path. Put the object and
executable in the caller's build directory. Assertions must remain enabled.

The zero setting matches the Windows WebRTC archive. MSVC's `std::runtime_error`
borrows the source message with that setting; `BridgeError` must retain its own
storage in both settings. Do not fix this by changing the prebuilt archive's
STL configuration or disabling Rust's undefined-behavior checks.

`cargo test -p libwebrtc --test bridge_errors` exercises the actual CXX bridge,
verified-key round trip, wrong/unknown key rejection, all four key-provider
failure paths, and temporary SDP error serialization. No LiveKit server is
required. On Windows use the archive's static CRT (`-C target-feature=+crt-static`).
