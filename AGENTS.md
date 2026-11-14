# Project instructions

HDContext is a Rust desktop app for finding, reviewing, and exporting highlights from game recordings. Read [README.md](README.md) for the user workflow and [docs/architecture.md](docs/architecture.md) before changing shared analysis or storage behavior.

## Build and checks

Use Rust stable with the 2024 edition. Native dependencies include FFmpeg/ffprobe, GStreamer development libraries and playback plugins, pkg-config, and the desktop dependencies listed in [docs/development.md](docs/development.md).

```sh
cargo fmt --check
cargo clippy -j 1 --all-targets -- -D warnings
cargo test -j 1 -- --test-threads=1
cargo build --release -j 1
```

The existing development workspace benefits from one build job and serial tests because native decoding and concurrent linking have produced intermittent failures. If Cargo is missing from PATH, use `$HOME/.cargo/bin/cargo` when present. Do not assume another machine has the same configuration.

Tests use temporary files, synthetic recordings, local mock HTTP servers, and a silent audio sink. The real credential-store roundtrip test is intentionally ignored. Never run it, send real recordings to a provider, or start an automatic watcher as part of routine validation.

## Product behavior

- Keep the library, source playback, and highlight editing in one native window.
- In the normal editor, controls and timelines stay below the video and remain visible. The video must not extend underneath them.
- Viewing mode fills the workspace and shows only basic playback, volume/mute, the two timelines, and the selected highlight name. Its overlay hides during idle playback and returns on interaction. Escape or Back to editor restores the editor.
- All highlights share one track below the recording ruler. Clicking selects a highlight; overlapping blocks can be cycled. Editing supports moving blocks, trimming boundaries, and undo/redo.
- Preserve keyboard shortcuts, including Space and comma/period frame stepping. Do not intercept typing, numeric input, open menus, or confirmation dialogs.
- Provider analysis returns short event labels and source-relative time ranges, not general feedback on the whole recording.
- Playback starts without waiting for a transcode or source hashing. Analysis, source verification, exports, and credential operations run off the UI thread.
- Apply background results without restarting playback or discarding concurrent manual edits.

## Shared state and compatibility

- Resolve folder context through `context.rs` in both GUI and headless scans. Keep explicit empty overrides; do not replace them with inferred names.
- Saved preferences, scan-cache results, and review files have different purposes. Manual edits and keeper choices require a saved review.
- Keep Gemini and OpenAI credentials and models separate. Never switch providers, repair results through extra paid calls, or retry provider limits implicitly.
- Preserve compatible reading of version-1 preferences and reports, legacy filename reports, and point highlights. Add serde defaults and migration tests when evolving saved state.
- GUI and watcher requests share scan-cache locks. Respect provider pauses, source changes, and more specific watched-folder profiles.

## File and credential safety

- Never commit keys, `.env` files, local settings, scan caches, review reports, or user recordings. Do not print credentials or raw provider error bodies.
- Provider keys belong in the operating system credential store or environment, never preferences, reports, unit files, or a plaintext fallback.
- Do not modify user recordings while testing. Use temporary synthetic sources.
- Keep-originals export is the default. Replacement must stay an explicit export-dialog choice with a clearly labelled destructive action.
- Replacement renders and verifies every selected output for a source before removing that source. Preserve no-clobber publication, source checks, scan locks, output caching, and date-preservation behavior.
- A failure must not overwrite existing outputs or delete the source before verified outputs exist. Do not weaken these guarantees for performance.

## Working on changes

Use the existing Rust modules and native egui/GStreamer stack. Avoid adding a browser player, floating player window, or new UI framework. Keep blocking operations away from render methods. Test behavior and failure boundaries rather than duplicating implementation details.

Update the relevant documentation when changing UI flows, CLI flags, persisted state, provider requests, watcher lifecycle, or exports. Native screenshot validation uses the isolated fixture in `examples/ui_review.rs`; see [docs/development.md](docs/development.md). It must remain independent of real preferences, credentials, and API traffic.

Commit or push when the user asks. Do not install services, restart live watchers, make live API calls, or alter machine configuration merely to validate a code change.
