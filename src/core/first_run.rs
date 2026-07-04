//! First-run source picker.
//!
//! Historically spotatui forced a Spotify OAuth login before the TUI could open.
//! Now that YouTube, Subsonic/Navidrome, Internet Radio, and Local Files are all
//! free, first launch instead asks which source to set up. Picking Spotify falls
//! through to the existing auth wizard; picking a free source seeds a default
//! `client.yml` (so Spotify can still be added later via in-TUI login), records
//! the choice as the active source, and collects any source-specific config.
//!
//! Only sources whose Cargo feature is compiled in are offered. A build with just
//! Spotify (slim / current macOS release) shows no picker and keeps the original
//! behavior.

use crate::core::config::ClientConfig;
use crate::core::source::Source;
use crate::core::user_config::UserConfig;
use anyhow::{anyhow, Result};
use std::io::{stdin, stdout, Write};

/// Run the interactive first-run source picker. A no-op after the first launch
/// (detected by the presence of `client.yml`) and when only Spotify is compiled
/// in. Must be called before [`ClientConfig::load_config`], which would otherwise
/// trigger the Spotify-only auth wizard on a fresh install.
pub async fn run_first_run_picker(
  user_config: &mut UserConfig,
  client_config: &mut ClientConfig,
) -> Result<()> {
  // First run is detected by the absence of the Spotify client config file.
  let paths = client_config.get_or_build_paths()?;
  if paths.config_file_path.exists() {
    return Ok(());
  }

  let options = compiled_in_sources();

  // Only Spotify available (slim build / macOS release without the source
  // features): keep today's behavior and let `load_config` run the wizard.
  if options.len() == 1 {
    return Ok(());
  }

  println!("\nWelcome to spotatui! Choose your music source:\n");
  for (index, source) in options.iter().enumerate() {
    println!(
      "  {}) {}{}",
      index + 1,
      source.label(),
      source_note(*source)
    );
  }
  println!("\nYou can add or switch sources anytime from the `d` menu.");

  let choice = prompt_choice(options.len())?;
  let selected = options[choice - 1];

  if selected == Source::Spotify {
    // Fall through: `load_config` runs the existing Spotify auth wizard.
    return Ok(());
  }

  // Free source: seed a default `client.yml` (no OAuth) so a later in-TUI Spotify
  // login has a client id to work with, record the active source, then collect
  // any credentials the source needs.
  client_config.init_default_spotify_config()?;
  user_config.behavior.active_source = selected;
  // Persisting the config here writes `enable_global_song_count`, which suppresses
  // the later opt-in prompt. Default it to opt-out so we never enable anonymous
  // telemetry for a user who was never asked (they can enable it in config.yml).
  user_config.behavior.enable_global_song_count = false;
  user_config.save_config()?;

  configure_source(selected, user_config).await?;

  println!(
    "\nStarting spotatui with {} as your source. Press `d` anytime to switch or to log in to Spotify.\n",
    selected.label()
  );

  Ok(())
}

/// The sources whose Cargo feature is compiled into this build, in display order.
/// Spotify is always present.
fn compiled_in_sources() -> Vec<Source> {
  // `mut` is unused in a Spotify-only (slim) build where every push is cfg'd out.
  #[cfg_attr(
    not(any(
      feature = "youtube",
      feature = "subsonic",
      feature = "internet-radio",
      feature = "local-files"
    )),
    allow(unused_mut)
  )]
  let mut options = vec![Source::Spotify];
  #[cfg(feature = "youtube")]
  options.push(Source::YouTube);
  #[cfg(feature = "subsonic")]
  options.push(Source::Subsonic);
  #[cfg(feature = "internet-radio")]
  options.push(Source::Radio);
  #[cfg(feature = "local-files")]
  options.push(Source::Local);
  options
}

fn source_note(source: Source) -> &'static str {
  match source {
    Source::Spotify => " (needs login)",
    Source::YouTube => " (free, needs the yt-dlp binary)",
    Source::Subsonic => " (free, needs a Subsonic/Navidrome server)",
    Source::Radio => " (free)",
    Source::Local => " (free)",
  }
}

// `user_config` is only read by credential/config-collecting sources; a build
// with none of them (slim) leaves it unused.
#[cfg_attr(
  not(any(feature = "subsonic", feature = "youtube", feature = "local-files")),
  allow(unused_variables)
)]
async fn configure_source(source: Source, user_config: &mut UserConfig) -> Result<()> {
  match source {
    #[cfg(feature = "subsonic")]
    Source::Subsonic => configure_subsonic(user_config).await?,
    #[cfg(feature = "youtube")]
    Source::YouTube => configure_youtube(user_config),
    #[cfg(feature = "local-files")]
    Source::Local => configure_local(user_config),
    // Radio needs no setup; other sources are handled above when compiled in.
    _ => {}
  }
  Ok(())
}

#[cfg(feature = "subsonic")]
async fn configure_subsonic(user_config: &mut UserConfig) -> Result<()> {
  println!("\nSubsonic / Navidrome setup:");
  let url = prompt_line("Server URL (e.g. https://demo.navidrome.org)")?;
  let username = prompt_line("Username")?;
  let password = prompt_line("Password")?;

  user_config.behavior.subsonic_url = Some(url.clone());
  user_config.behavior.subsonic_username = Some(username.clone());
  user_config.behavior.subsonic_password = Some(password.clone());
  user_config.save_config()?;

  // Best-effort connectivity check: a failure is not fatal (the server may just
  // be temporarily down), the details are already saved.
  print!("Testing connection... ");
  let _ = stdout().flush();
  let client = crate::infra::subsonic::SubsonicSource::new(url, username, password);
  match client.ping().await {
    Ok(()) => println!("OK"),
    Err(e) => {
      println!("failed: {e}");
      println!(
        "Saved anyway. Fix the details in ~/.config/spotatui/config.yml and relaunch if needed."
      );
    }
  }

  Ok(())
}

#[cfg(feature = "youtube")]
fn configure_youtube(user_config: &UserConfig) {
  let ytdlp = user_config
    .behavior
    .ytdlp_path
    .clone()
    .unwrap_or_else(|| "yt-dlp".to_string());

  print!("\nChecking for yt-dlp... ");
  let _ = stdout().flush();
  match std::process::Command::new(&ytdlp).arg("--version").output() {
    Ok(output) if output.status.success() => {
      let version = String::from_utf8_lossy(&output.stdout);
      println!("found ({})", version.trim());
    }
    _ => {
      println!("not found");
      println!("YouTube playback needs the `yt-dlp` binary on your PATH.");
      println!("Install it (e.g. `pipx install yt-dlp` or your distro package) and relaunch.");
      println!(
        "If it lives at a custom path, set behavior.ytdlp_path in ~/.config/spotatui/config.yml."
      );
    }
  }
}

#[cfg(feature = "local-files")]
fn configure_local(user_config: &UserConfig) {
  match &user_config.behavior.local_music_path {
    Some(path) => {
      println!("\nLocal files will be read from: {path}");
      println!("(Change behavior.local_music_path in config.yml to use another folder.)");
    }
    None => {
      println!("\nNo music folder was detected automatically.");
      println!("Set behavior.local_music_path in ~/.config/spotatui/config.yml.");
    }
  }
}

fn prompt_choice(max: usize) -> Result<usize> {
  const MAX_RETRIES: u8 = 5;
  let mut retries = 0;
  loop {
    print!("\nChoose (1-{max}): ");
    let _ = stdout().flush();
    let mut input = String::new();
    stdin().read_line(&mut input)?;
    match input.trim().parse::<usize>() {
      Ok(n) if (1..=max).contains(&n) => return Ok(n),
      _ => {
        println!("Invalid choice. Please enter a number between 1 and {max}.");
        retries += 1;
        if retries >= MAX_RETRIES {
          return Err(anyhow!("Maximum retries ({MAX_RETRIES}) exceeded."));
        }
      }
    }
  }
}

// Only credential-collecting sources (currently Subsonic) use this.
#[cfg_attr(not(feature = "subsonic"), allow(dead_code))]
fn prompt_line(label: &str) -> Result<String> {
  const MAX_RETRIES: u8 = 5;
  let mut retries = 0;
  loop {
    print!("  {label}: ");
    let _ = stdout().flush();
    let mut input = String::new();
    stdin().read_line(&mut input)?;
    let trimmed = input.trim().to_string();
    if !trimmed.is_empty() {
      return Ok(trimmed);
    }
    println!("  (required)");
    retries += 1;
    if retries >= MAX_RETRIES {
      return Err(anyhow!("Maximum retries ({MAX_RETRIES}) exceeded."));
    }
  }
}
