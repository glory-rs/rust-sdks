// Copyright 2025 LiveKit, Inc.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

#[cfg(feature = "__lk-e2e-test")]
use {
    anyhow::{anyhow, bail, Result},
    common::{
        audio::{FreqAnalyzer, SineParameters, SineTrack},
        test_rooms,
    },
    futures_util::StreamExt,
    libwebrtc::audio_stream::native::NativeAudioStream,
    livekit::{prelude::*, SimulateScenario},
    std::{sync::Arc, time::Duration},
    tokio::{sync::mpsc::UnboundedReceiver, time::timeout},
};

mod common;

#[cfg(feature = "__lk-e2e-test")]
const SINE_FREQ: f64 = 440.0;

/// Waits for the next subscribed remote audio track and checks that it carries
/// the publisher's sine tone.
#[cfg(feature = "__lk-e2e-test")]
async fn assert_receives_sine(events: &mut UnboundedReceiver<RoomEvent>) -> Result<TrackSid> {
    let track = loop {
        let Some(event) = events.recv().await else { bail!("event stream ended") };
        if let RoomEvent::TrackSubscribed { track: RemoteTrack::Audio(track), .. } = event {
            break track;
        }
    };
    let sid = track.sid();

    let mut stream = NativeAudioStream::new(track.rtc_track(), 48_000, 1);
    let mut analyzer = FreqAnalyzer::new();
    let mut frames = 0;
    // Skip silence/concealment at stream start, then measure ~1 s of audio.
    while let Some(frame) = stream.next().await {
        if frames == 0 && !frame.data.iter().any(|&s| s.unsigned_abs() >= i16::MAX as u16 / 10) {
            continue;
        }
        frames += 1;
        if frames <= 50 {
            continue;
        }
        analyzer.analyze(frame.data.iter().copied());
        if frames >= 150 {
            break;
        }
    }
    let freq = analyzer.estimated_freq(48_000);
    if (freq - SINE_FREQ).abs() > 20.0 {
        bail!("expected ~{SINE_FREQ} Hz on {sid}, measured {freq:.1} Hz");
    }
    Ok(sid)
}

/// A full reconnect closes the old session, which now detaches every publisher
/// sender before closing the PeerConnection. The republish that follows must
/// still succeed and the subscriber must hear the track again.
#[cfg(feature = "__lk-e2e-test")]
#[test_log::test(tokio::test)]
async fn test_full_reconnect_republishes_audio() -> Result<()> {
    let mut rooms = test_rooms(2).await?;
    let (pub_room, mut pub_events) = rooms.remove(0);
    let (_sub_room, mut sub_events) = rooms.remove(0);
    let pub_room = Arc::new(pub_room);

    let params =
        SineParameters { sample_rate: 48_000, freq: SINE_FREQ, amplitude: 1.0, num_channels: 1 };
    let mut sine = SineTrack::new(pub_room.clone(), params);
    sine.publish().await?;

    let first_sid = timeout(Duration::from_secs(15), assert_receives_sine(&mut sub_events))
        .await
        .map_err(|_| anyhow!("subscriber never heard the initial track"))??;

    pub_room
        .simulate_scenario(SimulateScenario::FullReconnect)
        .await
        .map_err(|e| anyhow!("simulate_scenario failed: {e:?}"))?;

    let (previous_sid, new_sid) = timeout(Duration::from_secs(30), async {
        while let Some(event) = pub_events.recv().await {
            match event {
                RoomEvent::LocalTrackRepublished { previous_sid, publication, .. } => {
                    return Ok((previous_sid, publication.sid()));
                }
                RoomEvent::Disconnected { reason } => bail!("disconnected: {reason:?}"),
                _ => {}
            }
        }
        bail!("event stream ended before republish")
    })
    .await
    .map_err(|_| anyhow!("track was never republished"))??;

    assert_eq!(previous_sid, first_sid);
    assert_ne!(new_sid, first_sid, "a full reconnect should assign a new track sid");
    assert_eq!(pub_room.connection_state(), ConnectionState::Connected);

    let publications = pub_room.local_participant().track_publications();
    assert_eq!(publications.len(), 1, "exactly the republished track should remain");
    assert!(publications.contains_key(&new_sid));

    let resubscribed_sid = timeout(Duration::from_secs(20), assert_receives_sine(&mut sub_events))
        .await
        .map_err(|_| anyhow!("subscriber never heard the republished track"))??;
    assert_eq!(resubscribed_sid, new_sid);

    pub_room.close().await?;
    Ok(())
}
