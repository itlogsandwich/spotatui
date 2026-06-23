//! Local-file playback routing.
//!
//! This is the seam that keeps the Spotify [`Network`](crate::infra::network)
//! Spotify-only: [`route_playback_event`] is called from the runtime IoEvent
//! pump *before* `handle_network_event`. When the event targets local playback
//! (a `file://` URI, or any transport control while a local file owns the
//! session) it is handled here against the live [`LocalPlayer`] and the event is
//! consumed; otherwise it falls through to the normal Spotify dispatch.
//!
//! ## Decoupling
//!
//! Local playback owns a single piece of state, [`App::local_playback`]. This
//! module never writes Spotify/librespot fields (`native_track_info`,
//! `song_progress_ms`, `is_streaming_active`, …): the playbar reads progress and
//! pause state live from the player, so the two playback worlds cannot desync.
//!
//! ## Device ownership
//!
//! Only one backend holds the audio output device at a time (required on
//! exclusive-ALSA setups, harmless elsewhere). Starting local playback pauses
//! native Spotify (librespot releases the device when its sink stops); starting
//! Spotify tears the local session down (dropping it releases the device).
//!
//! ## Publish-once
//!
//! `local_playback` is set exactly once, in the success arm of [`start_local`],
//! *after* the source is decoding. While it is `None` neither the playbar nor
//! the runtime tick touch local state, so the brief "opening" window is simply
//! invisible — there is no half-initialised state for a tick to misread.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Mutex;

use super::player::LocalPlayer;
use super::{file_uri_to_path, track_info_from_path, LocalPlaybackState};
use crate::core::app::App;
use crate::infra::network::IoEvent;

/// Whether a URI is owned by the local-files source.
fn is_file_uri(uri: &str) -> bool {
  uri.starts_with("file:")
}

/// Intercept playback events that target local files.
///
/// Returns `true` if the event was handled locally (and must **not** be
/// forwarded to the Spotify network), `false` to let the normal dispatch run.
pub async fn route_playback_event(app: &Arc<Mutex<App>>, event: &IoEvent) -> bool {
  match event {
    // Start playing a local file.
    IoEvent::StartPlayback(Some(uri), _, _) if is_file_uri(uri) => {
      start_local(app, uri).await;
      true
    }
    // Bare "resume current" — ours only while a local file owns the session.
    IoEvent::StartPlayback(None, None, None) => match player(app).await {
      Some(player) => {
        player.resume();
        true
      }
      None => false,
    },
    // Any other start is a real Spotify play: relinquish the device first, then
    // let the network handle it.
    IoEvent::StartPlayback(..) => {
      teardown_local(app).await;
      false
    }
    IoEvent::PausePlayback => match player(app).await {
      Some(player) => {
        player.pause();
        true
      }
      None => false,
    },
    IoEvent::Seek(position_ms) => match player(app).await {
      Some(player) => {
        // The playbar reads position live from the player, so the seek shows up
        // on the next render with nothing else to update.
        let _ = player.seek(Duration::from_millis(*position_ms as u64));
        true
      }
      None => false,
    },
    IoEvent::ChangeVolume(volume) => match player(app).await {
      Some(player) => {
        player.set_volume(*volume as f32 / 100.0);
        // Keep the playbar's volume readout in sync.
        app.lock().await.user_config.behavior.volume_percent = *volume;
        true
      }
      None => false,
    },
    // Single-file local playback has no queue yet; swallow skips so they don't
    // reach (and disturb) Spotify.
    IoEvent::NextTrack | IoEvent::PreviousTrack | IoEvent::ForcePreviousTrack => {
      app.lock().await.local_playback.is_some()
    }
    _ => false,
  }
}

/// The live local player, if a local file currently owns the session.
async fn player(app: &Arc<Mutex<App>>) -> Option<Arc<LocalPlayer>> {
  app
    .lock()
    .await
    .local_playback
    .as_ref()
    .map(|local| Arc::clone(&local.player))
}

/// Begin playing the local file at `uri`, taking over the playback session.
async fn start_local(app: &Arc<Mutex<App>>, uri: &str) {
  let path = match file_uri_to_path(uri) {
    Ok(path) => path,
    Err(e) => {
      set_error(app, format!("Invalid local file URI: {e}")).await;
      return;
    }
  };

  // Pause native Spotify so librespot releases the output device.
  #[cfg(feature = "streaming")]
  {
    let streaming = app.lock().await.streaming_player.clone();
    if let Some(player) = streaming {
      player.pause();
    }
  }

  let player = match acquire_player(app).await {
    Some(player) => player,
    None => return, // error already surfaced
  };

  // Tag reading and decoder construction are blocking file I/O — keep them off
  // the async executor.
  let decode_path = path.clone();
  let decode_player = Arc::clone(&player);
  let result = tokio::task::spawn_blocking(move || {
    let info = track_info_from_path(&decode_path);
    decode_player.play_file(&decode_path).map(|()| info)
  })
  .await;

  match result {
    Ok(Ok(info)) => {
      let volume = app.lock().await.user_config.behavior.volume_percent;
      player.set_volume(volume as f32 / 100.0);

      // Publish the session exactly once, now that the source is decoding.
      let display_name = info.name.clone();
      let mut app = app.lock().await;
      app.local_playback = Some(LocalPlaybackState {
        player,
        name: info.name,
        artists: info.artists.join(", "),
        album: info.album,
        duration_ms: info.duration_ms,
      });
      app.set_status_message(format!("\u{266a} {display_name}"), 4);
    }
    Ok(Err(e)) => set_error(app, format!("Cannot play local file: {e}")).await,
    Err(e) => set_error(app, format!("Local playback task failed: {e}")).await,
  }
}

/// Reuse the live player if a local file is already playing, otherwise open the
/// output device for a fresh one. A freshly opened player is **not** published
/// to `App` here — [`start_local`] publishes it only on success, so there is no
/// window where `local_playback` is `Some` with an empty sink.
async fn acquire_player(app: &Arc<Mutex<App>>) -> Option<Arc<LocalPlayer>> {
  if let Some(player) = player(app).await {
    return Some(player);
  }

  match tokio::task::spawn_blocking(LocalPlayer::new).await {
    Ok(Ok(player)) => Some(Arc::new(player)),
    Ok(Err(e)) => {
      set_error(app, format!("No audio output for local playback: {e}")).await;
      None
    }
    Err(e) => {
      set_error(app, format!("Audio output init failed: {e}")).await;
      None
    }
  }
}

/// End the local session, releasing the output device.
async fn teardown_local(app: &Arc<Mutex<App>>) {
  if let Some(local) = app.lock().await.local_playback.take() {
    local.player.stop();
    // `local` is dropped here; if it held the last reference the keepalive
    // thread exits and the output device is released.
  }
}

async fn set_error(app: &Arc<Mutex<App>>, message: String) {
  app.lock().await.set_status_message(message, 6);
}
