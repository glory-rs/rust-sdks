// Copyright 2025 LiveKit, Inc.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not #![cfg(not(target_arch = "wasm32"))]

use libwebrtc::{
    native::frame_cryptor::{
        DataPacketCryptor, EncryptionAlgorithm, KeyDerivationAlgorithm, KeyProvider,
        KeyProviderOptions,
    },
    peer_connection_factory::PeerConnectionFactory,
    session_description::{SdpType, SessionDescription},
};

fn provider(shared_key: bool) -> KeyProvider {
    KeyProvider::new(KeyProviderOptions {
        shared_key,
        ratchet_window_size: 16,
        ratchet_salt: b"LKFrameEncryptionKey".to_vec(),
        failure_tolerance: -1,
        key_ring_size: 16,
        key_derivation_algorithm: KeyDerivationAlgorithm::PBKDF2,
    })
}

#[test]
fn packet_and_provider_failures_preserve_owned_errors_across_cxx() {
    let _factory = PeerConnectionFactory::default();
    let sender = provider(false);
    let receiver = provider(false);
    assert!(sender.set_key("alice".into(), 0, vec![7; 32]));
    assert!(receiver.set_key("alice".into(), 0, vec![7; 32]));
    assert!(receiver.set_key("bob".into(), 0, vec![8; 32]));
    let encryptor = DataPacketCryptor::new(EncryptionAlgorithm::AesGcm, sender.clone());
    let decryptor = DataPacketCryptor::new(EncryptionAlgorithm::AesGcm, receiver);
    let shared = provider(true);
    let mut retained = Vec::new();
    for index in 0..32 {
        let plaintext = format!("private packet {index}");
        let packet = encryptor.encrypt("alice", 0, plaintext.as_bytes()).unwrap();
        assert_ne!(packet.data, plaintext.as_bytes());
        assert_eq!(decryptor.decrypt("alice", &packet).unwrap(), plaintext.as_bytes());
        for participant in ["bob", "unknown"] {
            let error = decryptor.decrypt(participant, &packet).unwrap_err();
            let message = error.to_string();
            assert!(message.starts_with("Decryption failed: Failed to decrypt data packet: "));
            retained.push((error, message));
        }
        let error = encryptor.encrypt("unknown", 0, plaintext.as_bytes()).unwrap_err();
        let message = error.to_string();
        assert!(message.starts_with("Encryption failed: Failed to encrypt data packet: "));
        retained.push((error, message));
        assert!(sender.get_key("unknown".into(), 0).is_none());
        assert!(sender.ratchet_key("unknown".into(), 0).is_none());
        assert!(shared.get_shared_key(0).is_none());
        assert!(shared.ratchet_shared_key(0).is_none());
    }
    for (error, message) in retained {
        assert_eq!(error.to_string(), message);
    }
}

#[test]
fn malformed_sdp_error_survives_its_cpp_temporary_and_later_parse_errors() {
    let mut errors = Vec::new();
    for index in 0..128 {
        let invalid = format!("not-an-sdp-line-{index}{}", "x".repeat(128));
        let error = SessionDescription::parse(&invalid, SdpType::Offer).unwrap_err();
        assert!(!error.description.is_empty());
        let description = error.description.clone();
        let line = error.line.clone();
        errors.push((error, description, line));
    }
    for (error, description, line) in errors {
        assert_eq!(error.description, description);
        assert_eq!(error.line, line);
    }
}
