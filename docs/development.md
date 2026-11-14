# Development

## Requirements

Use a stable Rust toolchain with edition-2024 support. Keep `Cargo.lock` committed so application builds resolve the same dependency versions.

Building and running require:

- A C toolchain and pkg-config.
- FFmpeg and FFprobe on PATH, including H.264 and AAC encoding support.
- GStreamer and its development headers, Base/Good playback plugins, and compatible decoders such as libav.
- A desktop session with X11 or Wayland and OpenGL for the native GUI.
- On Linux, D-Bus development files for Secret Service and an XDG desktop portal backend for file dialogs.

See the [README](../README.md#start-the-app) for package names. On the original development machine, several Good plugins were installed in the user's GStreamer plugin directory. That is a local workaround, not a project prerequisite; prefer your distribution's normal packages.

## Build and run

```sh
cargo build --locked -j 1
cargo run --locked -- gui
cargo build --locked --release -j 1
./target/release/hdcontext
```

An ordinary GUI launch restores saved provider settings. `gui --free-tier` deliberately selects the Gemini preset. Opening an unscanned video can start provider traffic when Analyze on open is enabled; turn it off for local-only manual inspection.

Use `cargo run -- --help` and `cargo run -- analyze --help` for CLI flags. `doctor` checks local requirements and whether key environment variables are set, without validating a key or calling a provider. It does not query stored desktop credentials.

## Checks

```sh
cargo fmt --check
cargo clippy -j 1 --all-targets -- -D warnings
cargo test -j 1 -- --test-threads=1
cargo doc --no-deps
```

Serial execution is useful in the original development environment, where concurrent native playback tests and linking have occasionally failed. A test failure must still be investigated; do not assume every decoder failure is environmental.

The suite covers provider request bodies and error redaction through local mock HTTP servers, source-time preservation, exports, cache locking, context migration, settings persistence, UI gestures, frame decoding, and keyboard behavior. Media tests generate temporary synthetic sources with FFmpeg. Playback tests use a silent GStreamer sink.

The credential-store roundtrip test is ignored because it uses the real desktop credential store. Routine checks require no provider key or paid API call. Do not enable ignored tests against a user's environment without an explicit reason and authorization.

## Native visual review

The debug-only `ui_review` example creates an isolated desktop fixture with synthetic results. It never loads real preferences or credentials and does not issue provider requests.

Create a synthetic video long enough for the fixture's 163-second highlight endpoint:

```sh
ffmpeg -v error -nostdin -n -f lavfi \
  -i 'testsrc2=size=960x540:rate=24:duration=180' \
  -c:v libx264 -preset ultrafast -pix_fmt yuv420p /tmp/hdcontext-demo.mp4
cargo build --example ui_review -j 1
./target/debug/examples/ui_review /tmp/hdcontext-demo.mp4 /tmp/editor.png editor 1440 960
./target/debug/examples/ui_review /tmp/hdcontext-demo.mp4 /tmp/viewing.png expanded 1440 960
./target/debug/examples/ui_review /tmp/hdcontext-demo.mp4 /tmp/hidden.png hidden 1440 960
```

The command refuses to overwrite the demo recording. Use another path if it already exists. Screenshot output paths should be disposable files.

Fixture states include `editor`, `expanded`, `hidden`, `settings`, `folders`, `storage`, `empty`, `error`, `zoom`, `export`, and `replace`. `expanded` shows the simplified Viewing mode. `hidden` waits for its idle overlay to disappear and simulates an inactive pointer. The fixture closes after capturing its framebuffer.

On desktops that throttle background Wayland windows, prefix the example with:

```sh
env -u WAYLAND_DISPLAY WINIT_UNIX_BACKEND=x11
```

Append the example executable and arguments to that command. This is a native framebuffer capture, not a browser preview or a screenshot of the user's desktop.

## Making changes

Use [the architecture map](architecture.md#module-map) to locate shared behavior. Keep work that blocks on files, credentials, media tools, or HTTP outside the UI render path. For changes to exports, migrations, claims, or provider requests, add tests for the relevant failure boundaries. Small presentation-only edits can be checked with the existing interaction suite and isolated visual fixture.

Check both a normal editor and compact layout when changing player controls. Confirm that normal controls remain below the video, Viewing mode remains small, typing retains focus during autosave, and menus/modals do not trigger playback shortcuts.

Do not commit generated `target/` output, personal media, settings, cache data, `.env` files, or real reviews. The repository ignore rules cover common local artifacts but do not replace checking the staged files.
