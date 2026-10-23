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

//! Server-ended teardown and reconnect coverage for detaching publisher senders
//! before the publisher PeerConnection is closed.

#![cfg(feature = "__lk-e2e-test")]

use {
    anyhow::{anyhow, bail, Result},
    common::{
        audio::{FreqAnalyzer, SineParameters, SineTrack},
        test_rooms, test_rooms_with_options,
        video::{SolidColorParams, SolidColorTrack},
    },
    futures_util::StreamExt,
    libwebrtc::{
        audio_stream::native::NativeAudioStream, native::create_random_uuid, rtp_sender::RtpSender,
        video_stream::native::NativeVideoStream,
    },
    livekit::{
        e2ee::{
            key_provider::{KeyProvider, KeyProviderOptions},
            EncryptionType,
        },
        options::VideoCodec,
        prelude::*,
        DisconnectReason, E2eeOptions, SimulateScenario,
    },
    livekit_api::services::room::RoomClient,
    livekit_token::{AccessToken, VideoGrants},
    std::{env, sync::Arc, time::Duration},
    tokio::{
        sync::mpsc::UnboundedReceiver,
        time::{self, timeout},
    },
};

mod common;

const SINE_FREQ: f64 = 440.0;

struct Server {
    url: String,
    api_key: String,
    api_secret: String,
}

impl Server {
    fn from_env() -> Self {
        Self {
            url: env::var("LIVEKIT_URL").unwrap_or_else(|_| "ws://localhost:7880".into()),
            api_key: env::var("LIVEKIT_API_KEY").unwrap_or_else(|_| "devkey".into()),
            api_secret: env::var("LIVEKIT_API_SECRET").unwrap_or_else(|_| "secret".into()),
        }
    }

    fn room_client(&self) -> RoomClient {
        RoomClient::with_api_key(
            &self.url.replacen("ws", "http", 1),
            &self.api_key,
            &self.api_secret,
        )
    }

    async fn connect(
        &self,
        room_name: &str,
        identity: &str,
    ) -> Result<(Room, UnboundedReceiver<RoomEvent>)> {
        let token = AccessToken::with_api_key(&self.api_key, &self.api_secret)
            .with_grants(VideoGrants {
                room_join: true,
                room: room_name.to_owned(),
                ..Default::default()
            })
            .with_identity(identity)
            .to_jwt()?;
        Ok(Room::connect(&self.url, &token, RoomOptions::default()).await?)
    }

    /// Deletes `room_name`, retrying while LiveKit Cloud does not know the new room yet.
    async fn delete_room(&self, room_name: &str) -> Result<()> {
        let client = self.room_client();
        for _ in 0..50 {
            match client.delete_room(room_name).await {
                Err(err) if err.to_string().contains("not_found") => {
                    time::sleep(Duration::from_millis(100)).await;
                }
                result => return Ok(result?),
            }
        }
        bail!("room {room_name} never became deletable")
    }
}

fn sine_params() -> SineParameters {
    SineParameters { sample_rate: 48_000, freq: SINE_FREQ, amplitude: 1.0, num_channels: 1 }
}

fn video_params() -> SolidColorParams {
    SolidColorParams { width: 640, height: 360, luma: 180 }
}

fn e2ee_options() -> RoomOptions {
    let key_provider =
        KeyProvider::with_shared_key(KeyProviderOptions::default(), b"pr1444".to_vec());
    let mut options = RoomOptions::default();
    options.encryption = Some(E2eeOptions { key_provider, encryption_type: EncryptionType::Gcm });
    options
}

async fn wait_disconnected(events: &mut UnboundedReceiver<RoomEvent>) -> Result<DisconnectReason> {
    timeout(Duration::from_secs(30), async {
        while let Some(event) = events.recv().await {
            match event {
                RoomEvent::Reconnecting => bail!("a server-ended room must not reconnect"),
                RoomEvent::Disconnected { reason } => return Ok(reason),
                _ => {}
            }
        }
        bail!("event stream ended before Disconnected")
    })
    .await
    .map_err(|_| anyhow!("room never disconnected"))?
}

async fn wait_republished(
    events: &mut UnboundedReceiver<RoomEvent>,
) -> Result<(TrackSid, TrackSid)> {
    timeout(Duration::from_secs(30), async {
        while let Some(event) = events.recv().await {
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
    .map_err(|_| anyhow!("track was never republished"))?
}

async fn next_subscribed(events: &mut UnboundedReceiver<RoomEvent>) -> Result<RemoteTrack> {
    timeout(Duration::from_secs(20), async {
        while let Some(event) = events.recv().await {
            if let RoomEvent::TrackSubscribed { track, .. } = event {
                return Ok(track);
            }
        }
        bail!("event stream ended before a track was subscribed")
    })
    .await
    .map_err(|_| anyhow!("no track was subscribed"))?
}

/// Reads ~1 s of steady-state audio and checks it carries the sine tone.
async fn assert_hears_sine(stream: &mut NativeAudioStream) -> Result<()> {
    let measure = async {
        let mut analyzer = FreqAnalyzer::new();
        let mut frames = 0;
        while let Some(frame) = stream.next().await {
            if frames == 0 && !frame.data.iter().any(|&s| s.unsigned_abs() >= i16::MAX as u16 / 10)
            {
                continue;
            }
            frames += 1;
            if frames <= 50 {
                continue;
            }
            analyzer.analyze(frame.data.iter().copied());
            if frames >= 150 {
                return Ok(analyzer.estimated_freq(48_000));
            }
        }
        bail!("audio stream ended")
    };
    let freq = timeout(Duration::from_secs(20), measure)
        .await
        .map_err(|_| anyhow!("no audible sine received"))??;
    if (freq - SINE_FREQ).abs() > 20.0 {
        bail!("expected ~{SINE_FREQ} Hz, measured {freq:.1} Hz");
    }
    Ok(())
}

async fn assert_receives_video(track: RemoteTrack) -> Result<()> {
    let RemoteTrack::Video(track) = track else { bail!("expected a video track") };
    let mut stream = NativeVideoStream::new(track.rtc_track());
    timeout(Duration::from_secs(20), async {
        for _ in 0..10 {
            let frame = stream.next().await.ok_or_else(|| anyhow!("video stream ended"))?;
            if frame.buffer.width() == 0 || frame.buffer.height() == 0 {
                bail!("received an empty video frame");
            }
        }
        Ok(())
    })
    .await
    .map_err(|_| anyhow!("no video frames received"))?
}

fn holding_track(senders: &[RtpSender]) -> usize {
    senders.iter().filter(|sender| sender.track().is_some()).count()
}

/// Polls senders taken before teardown until none holds a track, with the `Room` still alive.
async fn assert_senders_released(senders: &[RtpSender]) -> Result<()> {
    timeout(Duration::from_secs(5), async {
        while holding_track(senders) > 0 {
            time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .map_err(|_| {
        anyhow!(
            "{} publisher sender(s) still hold a track after the server ended the room",
            holding_track(senders)
        )
    })
}

/// Publishes one audio and one video track, ends the session with `end`, and
/// checks the senders let go of both tracks while the `Room` is still held.
async fn server_ended_releases_tracks(
    end: impl AsyncFnOnce(&Server, &str) -> Result<()>,
    expected_reason: DisconnectReason,
) -> Result<()> {
    let server = Server::from_env();
    let room_name = format!("test_room_{}", create_random_uuid());
    let (room, mut events) = server.connect(&room_name, "p0").await?;
    let room = Arc::new(room);

    let mut sine = SineTrack::new(room.clone(), sine_params());
    sine.publish().await?;
    let mut video = SolidColorTrack::new(room.clone(), video_params());
    video.publish(VideoCodec::VP8, true).await?;
    let senders = room.publisher_senders();
    assert_eq!(holding_track(&senders), 2);

    end(&server, &room_name).await?;

    assert_eq!(wait_disconnected(&mut events).await?, expected_reason);
    assert_eq!(room.connection_state(), ConnectionState::Disconnected);
    assert_senders_released(&senders).await
}

#[test_log::test(tokio::test)]
async fn test_room_deleted_releases_tracks_while_room_held() -> Result<()> {
    server_ended_releases_tracks(
        async |server, room_name| server.delete_room(room_name).await,
        DisconnectReason::RoomDeleted,
    )
    .await
}

#[test_log::test(tokio::test)]
async fn test_participant_removed_releases_tracks() -> Result<()> {
    server_ended_releases_tracks(
        async |server, room_name| {
            Ok(server.room_client().remove_participant(room_name, "p0").await?)
        },
        DisconnectReason::ParticipantRemoved,
    )
    .await
}

#[test_log::test(tokio::test)]
async fn test_duplicate_identity_releases_tracks() -> Result<()> {
    server_ended_releases_tracks(
        async |server, room_name| {
            // Leak the second connection's handle so it stays joined for the assertion.
            let (duplicate, _events) = server.connect(room_name, "p0").await?;
            std::mem::forget(duplicate);
            Ok(())
        },
        DisconnectReason::DuplicateIdentity,
    )
    .await
}

/// Full reconnect closes the old session, detaching its senders, then republishes
/// through them. Simulcast video must come back and reach the subscriber.
#[test_log::test(tokio::test)]
async fn test_full_reconnect_republishes_simulcast_video() -> Result<()> {
    let mut rooms = test_rooms(2).await?;
    let (pub_room, mut pub_events) = rooms.remove(0);
    let (_sub_room, mut sub_events) = rooms.remove(0);
    let pub_room = Arc::new(pub_room);

    let mut video = SolidColorTrack::new(pub_room.clone(), video_params());
    video.publish(VideoCodec::VP8, true).await?;
    assert_receives_video(next_subscribed(&mut sub_events).await?).await?;

    pub_room
        .simulate_scenario(SimulateScenario::FullReconnect)
        .await
        .map_err(|e| anyhow!("simulate_scenario failed: {e:?}"))?;
    let (_, new_sid) = wait_republished(&mut pub_events).await?;

    let track = next_subscribed(&mut sub_events).await?;
    assert_eq!(track.sid(), new_sid);
    assert_receives_video(track).await?;
    assert_eq!(holding_track(&pub_room.publisher_senders()), 1);
    Ok(())
}

/// Resume keeps the session, so it must not detach anything: same sid, audio keeps flowing.
#[test_log::test(tokio::test)]
async fn test_resume_keeps_publishing_audio() -> Result<()> {
    let mut rooms = test_rooms(2).await?;
    let (pub_room, mut pub_events) = rooms.remove(0);
    let (_sub_room, mut sub_events) = rooms.remove(0);
    let pub_room = Arc::new(pub_room);

    let mut sine = SineTrack::new(pub_room.clone(), sine_params());
    sine.publish().await?;
    let RemoteTrack::Audio(track) = next_subscribed(&mut sub_events).await? else {
        bail!("expected an audio track")
    };
    let sid = track.sid();
    let mut stream = NativeAudioStream::new(track.rtc_track(), 48_000, 1);
    assert_hears_sine(&mut stream).await?;

    pub_room
        .simulate_scenario(SimulateScenario::SignalReconnect)
        .await
        .map_err(|e| anyhow!("simulate_scenario failed: {e:?}"))?;
    timeout(Duration::from_secs(30), async {
        while let Some(event) = pub_events.recv().await {
            match event {
                RoomEvent::Reconnected => return Ok(()),
                RoomEvent::LocalTrackRepublished { .. } => bail!("resume must not republish"),
                RoomEvent::Disconnected { reason } => bail!("disconnected: {reason:?}"),
                _ => {}
            }
        }
        bail!("event stream ended before Reconnected")
    })
    .await
    .map_err(|_| anyhow!("room never resumed"))??;

    let publications = pub_room.local_participant().track_publications();
    assert_eq!(publications.len(), 1);
    assert!(publications.contains_key(&sid), "resume must keep the track sid");
    assert_eq!(holding_track(&pub_room.publisher_senders()), 1);
    assert_hears_sine(&mut stream).await
}

/// With E2EE, senders carry frame cryptors. Republish after a full reconnect must still
/// decrypt on the subscriber, and a server-ended room must still release the senders.
#[test_log::test(tokio::test)]
async fn test_e2ee_full_reconnect_and_server_end() -> Result<()> {
    let mut rooms = test_rooms_with_options([e2ee_options().into(), e2ee_options().into()]).await?;
    let (pub_room, mut pub_events) = rooms.remove(0);
    let (sub_room, mut sub_events) = rooms.remove(0);
    pub_room.e2ee_manager().set_enabled(true);
    sub_room.e2ee_manager().set_enabled(true);
    let room_name = pub_room.name();
    let pub_room = Arc::new(pub_room);

    let mut sine = SineTrack::new(pub_room.clone(), sine_params());
    sine.publish().await?;
    let RemoteTrack::Audio(track) = next_subscribed(&mut sub_events).await? else {
        bail!("expected an audio track")
    };
    assert_hears_sine(&mut NativeAudioStream::new(track.rtc_track(), 48_000, 1)).await?;

    pub_room
        .simulate_scenario(SimulateScenario::FullReconnect)
        .await
        .map_err(|e| anyhow!("simulate_scenario failed: {e:?}"))?;
    let (_, new_sid) = wait_republished(&mut pub_events).await?;
    let RemoteTrack::Audio(track) = next_subscribed(&mut sub_events).await? else {
        bail!("expected an audio track")
    };
    assert_eq!(track.sid(), new_sid);
    assert_hears_sine(&mut NativeAudioStream::new(track.rtc_track(), 48_000, 1)).await?;

    let senders = pub_room.publisher_senders();
    Server::from_env().room_client().delete_room(&room_name).await?;
    assert_eq!(wait_disconnected(&mut pub_events).await?, DisconnectReason::RoomDeleted);
    assert_senders_released(&senders).await
}
