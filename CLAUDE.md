# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build & Run

```bash
# Full build (native streaming + audio visualization)
cargo run

# Slim build — no librespot/audio; fastest iteration, used by CI
cargo run --no-default-features --features telemetry

# With the free alternative sources (Local/Subsonic/Radio/YouTube). These are NOT
# in `default`, so a plain `cargo run` is Spotify-only; use the `all-sources` alias
# (or list them individually) to exercise the first-run source picker and playback.
cargo run --features all-sources
```

## CI Checks (run before opening a PR)

```bash
cargo fmt --all
cargo clippy --no-default-features --features telemetry -- -D warnings
cargo test --no-default-features --features telemetry
```

## Run a Single Test

```bash
cargo test --no-default-features --features telemetry <test_name>
# Example:
cargo test --no-default-features --features telemetry global_shift_w_adds_current_track_from_anywhere
```

## Architecture

The codebase is split into four top-level modules under `src/`:

| Module | Role |
|--------|------|
| `core/` | Business logic & centralized state (`App`, `UserConfig`, `SortState`) |
| `infra/` | Infrastructure: Spotify API (`network/`), audio capture/viz (`audio/`), native streaming (`player/`), OS integrations (Discord RPC, MPRIS, macOS media keys) |
| `tui/` | Terminal UI: rendering (`ui/`), per-screen input handlers (`handlers/`), event loop (`event/`) |
| `cli/` | CLI argument parsing and self-update logic |

### Data flow

```
Key event → tui/event/ → tui/handlers/handle_app()
                           ↓ global keybindings
                           ↓ handle_block_events() dispatches to per-screen handler
                           ↓ app.dispatch(IoEvent::…) sends async work
                        infra/network/ fetches from Spotify API
                           ↓ mutates App state
                        tui/ui/ re-renders from App state
```

### Navigation / routing

`App` holds a navigation stack of `Route` values. Each `Route` contains:
- `RouteId` — which screen to render (Home, Search, Artist, AlbumTracks, Queue, Settings, Party, …)
- `ActiveBlock` — which block currently has keyboard focus
- `HoveredBlock` — which block the cursor is hovering

Use `app.push_navigation_stack(RouteId::X, ActiveBlock::X)` to navigate and `app.pop_navigation_stack()` to go back.

### Listening Party / sync

The Party feature (`src/infra/network/sync.rs`) connects host and guests via WebSocket relay using `SyncMessage` enums. `IoEvent::StartParty`, `JoinParty`, `SyncPlayback`, and `LeaveParty` drive the party lifecycle from handlers.

## Key Conventions

### Adding a new screen / feature

1. Add a variant to `RouteId` and `ActiveBlock` in `src/core/app.rs`.
2. Create `src/tui/handlers/<screen>.rs` with a `pub fn handler(key: Key, app: &mut App)` function and register it in `src/tui/handlers/mod.rs` (`handle_block_events` match arm).
3. Create `src/tui/ui/<screen>.rs` with a draw function and wire it into `src/tui/ui/mod.rs`.
4. Add any new Spotify API calls as `IoEvent` variants in `src/infra/network/mod.rs` and implement them in the appropriate `src/infra/network/<concern>.rs` file.

### Dispatching network calls

Call `app.dispatch(IoEvent::SomeVariant)` from a handler — never call async Spotify code directly from handlers or UI code.

### Paginated results

Use `ScrollableResultPages<T>` (defined in `src/core/app.rs`) for any data that comes back page-by-page from the Spotify API.

### Status messages

Show feedback with `app.show_status_message(msg, ttl_ms)`. Do not write directly to `app.status_message`.

### Dialog state cleanup

When closing a dialog, always call `app.clear_playlist_track_dialog_state()` alongside `app.dialog = None` and `app.confirm = false`.

### User-configurable keybindings

Always check `app.user_config.keys.<action>` instead of hard-coding key literals when matching global actions (see `handle_app` in `src/tui/handlers/mod.rs`).

### Feature flags

- Default features include `streaming` (librespot) and audio visualization backends.
- `--no-default-features --features telemetry` is the minimal build used for CI and fast iteration.
- Platform-specific audio backends (ALSA, PipeWire, PortAudio, Rodio) are gated behind their own features.
- `cover-art` feature enables album art rendering via `ratatui-image`.
