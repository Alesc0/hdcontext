<img src="assets/icon.svg" width="64" height="64" alt="">

# HDContext

Find the moments worth keeping in game recordings. Load a folder, skim for highlights with an AI model, and review short labels and playable time ranges such as `00:12.0–00:23.5  3-kill streak` or `00:04.0–00:09.0  Shotgun shot appears to miss`.

The library, video, and highlight timeline share one workspace. Click a recording to watch it immediately while the app searches for highlights in the background. The normal player shows highlight controls below the video. Adjust highlight ranges and export the moments you want to keep.

## Start the app

```sh
cargo build --locked --release -j 1
./target/release/hdcontext gui --free-tier
```

You need Rust stable, FFmpeg with H.264/AAC encoding, and GStreamer with its development libraries and playback plugins. Both `ffmpeg` and `ffprobe` must be on PATH. The native GUI needs X11 or Wayland and OpenGL on Linux. Only Linux has been tested.

GStreamer playback needs the Base and Good plugins, including `playbin`, `appsink`, `qtdemux`, audio output, and H.264/AAC decoders such as those in the libav plugin package. On Debian/Ubuntu, the relevant development/runtime packages include `libgstreamer1.0-dev`, `libgstreamer-plugins-base1.0-dev`, `gstreamer1.0-plugins-base`, `gstreamer1.0-plugins-good`, and `gstreamer1.0-libav`. On Arch, use `gstreamer`, `gst-plugins-base`, `gst-plugins-good`, and `gst-libav`. Compilation also needs a C compiler, pkg-config, Wayland, and xkbcommon development files. Native file/folder pickers on Linux use the XDG desktop portal and a file chooser backend for your desktop.

Build and native dependency details are in [Development](docs/development.md).

## Try the free model

1. Create a [Gemini API key](https://aistudio.google.com/api-keys) from a project marked **Free Tier** in [Google AI Studio](https://aistudio.google.com/projects).
2. Start the app with `gui --free-tier`, or open **Settings** and click **Use free-tier preset**.
3. Paste the Gemini key in **Settings**, and optionally add the game name as context.
4. Click **Open folder...** in the library, then click a recording. **Analyze on open** starts its search while you watch.

The preset selects `gemini-3.5-flash-lite`. Eligibility, pricing, and quotas depend on the model and your provider project. A preset does not guarantee free requests, and paid-project keys may incur charges. The app cannot verify or change your project's billing tier. See Google's [pricing](https://ai.google.dev/gemini-api/docs/pricing#gemini-3.5-flash-lite), [billing](https://ai.google.dev/gemini-api/docs/billing), and [data-use terms](https://ai.google.dev/gemini-api/terms).

Opening an unscanned recording starts an API request when **Analyze on open** is enabled and a key is available. Disable this toggle to watch without sending media. The preset never switches providers or models automatically.

## Review moments

Click a filename in the library to play the source video directly. Opening no longer waits for a transcode or a full-file hash. Source verification continues in the background. Playback requires GStreamer plugins for the recording's container and codecs. For recordings with multiple audio tracks, choose the playback track above the timeline. Analysis and exports still mix all audio tracks.

In the normal editor, the video and controls occupy separate areas. Controls remain visible below the video. Drag the divider or use **Playback → Video height** to resize the video; **Fill player space** fills the available space above the controls. Choose **Viewing mode** or double-click the video to fill the workspace and hide the library. Only in Viewing mode, the basic playback controls and timelines overlay the video and hide after 2.5 seconds of inactivity during playback. Move the mouse over the video to reveal them; pausing or interacting keeps them visible. Use **Back to editor** or **Escape** to restore the editing layout. Volume and mute are directly available in both modes.

With **Analyze on open** enabled, the app queues a background search for each unscanned recording you open. Searches run one at a time. You can watch, scrub, resize the video, edit highlights, and switch recordings while searching. Results appear on the timeline without restarting playback. Manual highlights and edits made during the search are retained. Already scanned recordings reuse their results; **Rescan highlights** inside the player explicitly requests a new scan. A failed search is not automatically retried, and a provider limit pauses automatic searches.

Usually the model proposes one main highlight, grouping related kills into one streak, or no highlights when nothing is clear. It may return up to six distinct events. All highlights appear together on one track below the recording timeline. Click a block to show its name and controls beneath the timeline. Repeated clicks on overlapping blocks cycle through those highlights.

Drag the divider below the video or use **Playback → Video height** to resize the picture area. The library and settings panels also resize horizontally. Playback stays embedded in the main page.

| Shortcut | Action |
| --- | --- |
| Space | Play or pause |
| `.` | Pause and advance one decoded frame |
| `,` | Pause and seek back one frame at the video's frame rate |
| Left / Right | Seek five seconds backward / forward |
| Home / End | Seek to the beginning / end |
| Ctrl+Z / Ctrl+Shift+Z | Undo / redo a highlight edit |

Shortcuts do not run while typing in text or numeric inputs, or while a menu or confirmation dialog is open. Backward stepping uses the video's negotiated frame rate, so variable-frame-rate recordings may have uneven steps.

Click a highlight block or use the **Highlights** menu to jump to it and show its name. **Fit selection** zooms the timeline for precise trimming; **Full clip** restores the recording timeline. Drag the middle of a highlight block to move the whole range while preserving its duration. Movement stops at the recording boundaries and supports undo/redo. Drag its left or right handle to resize the range, edit **Start** and **End** as timecodes or seconds, or use **More → Set start/end at playhead**. Undo/redo restores edits and marker removal. **More → Remove highlight** removes the marker, leaving media files intact. **Play selection** plays only that range. Edits immediately update the review and are included when you save a report. You can also add a highlight at the playhead, including in recordings that have not been analyzed.

Use **Export clip...** for the current highlight, or mark several moments **Keep** and click **Export keepers**. The export dialog lists the selected files and ranges and offers two modes:

- **Keep originals and save clips to a folder** is the default. Choose an output folder; original recordings stay in place.
- **Replace originals with the selected clips** saves the clips beside each source recording, then removes that original. Unselected footage is discarded. Each original is removed only after every selected clip from it has been rendered, verified, and saved. Confirm with **Export clips and remove originals**.

**Preserve recording date and file modified time** is enabled by default for both modes. The app copies the original filesystem modified time and stores its embedded `creation_time` in each MP4. If the recording has no valid embedded creation time, its filesystem modified time supplies the recording date. Windows also copies the original filesystem creation time when available. Linux assigns new filesystem creation times; it cannot copy those through the supported timestamp API. New files keep the same recording date rather than adding the highlight's start offset. Disable the checkbox to use new file dates.

Exports use edited ranges, H.264 video at source resolution/frame rate, and mixed AAC audio. Existing export files are never overwritten. Selected clips are staged in temporary directories and checked before publication. Invalid ranges, changed sources, collisions, cancellation before publication, and rendering failures keep the original. A failed original-file removal retains the successfully saved clips. Completed replacements of earlier originals in a batch remain completed if a later group fails. Playback reopens the first new clip, and the library removes the replaced source. Exported clips are cached as ready so watched folders do not send them for fresh AI analysis.

Previously exported moments are skipped by **Export keepers** until you edit their label or range. Earlier exported files stay unchanged. Exports require filesystem hard-link support to commit new files without overwriting existing ones.

Use the filter to find a source recording or event label. Use **Rescan highlights** inside the player to request another analysis of that recording. Watched recordings use their folder's provider, model, and game context. Rescanning makes new API requests and requires a fresh keeper selection; **Previous scans** retains earlier results and keeper choices.

Analysis and exports run in background tasks. Playback remains available throughout. Highlight edits remain available during analysis; exports wait for background work and source verification to finish. Edits are disabled during an export to keep its ranges consistent. **Stop** finishes the current request or export before stopping the remaining queue.

## Automatic folder scans

Open **Settings → Folders → Add watched folder...**. Each folder remembers its provider, model, optional audio model, game context, maximum recording duration, and subfolder setting. The initial values come from the current scan settings; you can edit each folder independently. Existing and incoming recordings become eligible after their size and modification time remain unchanged for six seconds. The watcher checks folders every two seconds between scans and processes one recording at a time. Temporary extensions such as `.part` are ignored. More specific folder configurations override recursive parent folders.

On Linux, adding the first watched folder also enables **Keep scanning when the app is closed**. This installs the app's `hdcontext-watch.service` as a systemd user service and starts it without a GUI. It starts again at login and keeps running while you are signed in. It does not enable system-wide services or keep the computer awake. Disabling the checkbox stops the service; watched folders then scan only while the GUI is open. Removing a folder or disabling **Scan automatically** stops new requests for it. An in-flight request may finish first.

The background watcher reads saved provider keys from the desktop credential store. Wait for the key to be remembered before closing the app. A missing key or locked credential store prevents scanning. API charges and provider quotas still apply. No keys go in the unit file, scan cache, or settings JSON.

Scan results are saved automatically under `scans/` beside `settings.json`. The GUI loads them into the library and timeline on the next launch, without uploading the recordings again. Source size and modification time distinguish replacement recordings. OS file locks prevent the GUI and background process from sending duplicate requests for a file. Failed or interrupted requests are not silently retried. A provider limit pauses its automatic queue; use **Resume paused automatic scans** after resolving the limit, and **Rescan highlights** for a failed recording. Player rescans of background-watched folders are queued for the watcher, so closing the GUI does not cancel them.

The scan cache saves AI results. Save a review to retain later manual trims, labels, keeper choices, and export history. Scanning leaves originals unchanged. The export dialog's replacement mode removes originals only after verified clips are saved.

For a manually managed headless watcher, run `hdcontext watch --config /path/to/settings.json`. Automatic service installation is currently available on Linux. Other platforms can run this command under their own process manager.

## Save a review

**File → Save review as...** writes a new JSON file with pending recordings, moment ranges, labels, keeper choices, export paths, earlier scans, provider/model settings, and context. API keys are excluded. **File → Open review...** restores it. Save again to a new path after further edits or exports; reports are not updated automatically. Closing or replacing an unsaved review prompts you to save it first.

Older filename-review reports still open. Old point highlights receive a small surrounding playback range; rescan them to get new moment-focused results. The legacy CLI rename command is still available for old workflows.

## Remembered settings and keys

The GUI automatically remembers the selected folder, subfolder option, provider, each provider's models, game/context text, maximum recording duration, the Analyze on open preference, and watched-folder configurations. Changes save shortly after you stop editing and are flushed again on exit. The folder path returns on launch. Loading the folder only lists files; opening an unscanned video starts its search when Analyze on open is enabled. **Save settings now** saves immediately.

API keys are remembered separately in the operating system's credential store: Secret Service on Linux, Keychain on macOS, and Credential Manager on Windows. On Linux, use an unlocked Secret Service provider such as GNOME Keyring or a compatible KWallet service. The `sync-secret-service` backend also needs the D-Bus development package at build time, such as `libdbus-1-dev` on Debian/Ubuntu. Credential operations run in a background thread.

Enter each provider's key once and wait for **Settings saved** without a credential error. The app restores it on the next launch. **Settings → Storage → Forget this provider's key** removes only the currently selected provider's stored key and clears its field. A nonempty key supplied by an environment variable takes precedence over a saved key; remove that environment variable as well if you want the key to stay forgotten after restarting.

If the credential store is locked or unavailable, the app shows the error and keeps the key in memory for that session. It never falls back to writing API keys into a settings file or review. Unlock the store and click **Retry storage** or **Save settings now**. A failed lookup does not delete an existing stored credential.

Non-secret preferences are saved to `settings.json` in the user's `hdcontext` configuration directory. On Linux this is `$XDG_CONFIG_HOME/hdcontext/settings.json`, or `~/.config/hdcontext/settings.json` when that variable is unset. macOS and Windows use their standard user configuration directory. The settings file is replaced atomically and uses private file permissions on Unix. Credentials are stored under the `io.hdcontext.desktop` service, separately for Gemini and OpenAI.

Game context is remembered separately for each folder. Open a folder or recording, then edit **Settings → Analysis → Game context**. The field shows which folder it applies to. Until you enter your own context, the app derives a suggestion from the folder name, such as `Helldivers2_Clips` becoming `Helldivers 2`. Dated subfolders inherit the parent context. You can edit the suggestion or clear it; an explicitly empty value stays empty. Manual scans and automatic folder scans use these same saved values. Existing global context is migrated to the previously selected folder only.

Launching with `gui --free-tier` intentionally overrides the restored provider/model with the Gemini preset. Launch with no arguments to restore your remembered choice unchanged. Reviews and keeper selections still use **File → Save review as...**; remembering app preferences does not automatically save a review.

## Providers

| Provider | Key environment variable | Analysis model | Audio model |
| --- | --- | --- | --- |
| Google Gemini | `GEMINI_API_KEY` | `gemini-3.5-flash-lite` | Combined video/audio input |
| OpenAI | `OPENAI_API_KEY` | `gpt-5.6-terra` | `gpt-audio-1.5` |

Select the provider in Settings → Analysis and paste its key, use its remembered credential, or supply the corresponding environment variable. Keys and model settings stay separate for each provider. The GUI remembers keys in the desktop credential store; they are never written into settings JSON or reviews. CLI analysis still reads its keys from the environment. The app does not load `.env` files automatically.

Change models in **Settings → Analysis → Models and limits**. A Gemini replacement must support video, audio, and structured output. OpenAI's visual model must support images and strict JSON Schema through Responses; its audio model must support WAV input and text output through Chat Completions. See [Gemini video input](https://ai.google.dev/gemini-api/docs/generate-content/video-understanding), [OpenAI image input](https://developers.openai.com/api/docs/guides/images-vision), and [OpenAI audio input](https://developers.openai.com/api/docs/guides/audio-chat-completions).

OpenAI requires available API credits. ChatGPT subscriptions and API billing are separate. If an API returns HTTP 429, the batch stops and completed results remain available. OpenAI errors identify recognized credit, spending, usage, or temporary rate-limit codes and the failing audio/image stage. Numeric Retry-After guidance is shown when available. No automatic retries or provider fallbacks are made.

## What the model sees

FFmpeg prepares the full timeline at 8 fps, with the game picture up to 960 by 540 pixels and mixed mono audio. A separate 64-pixel black band shows `CLIP_TIME HH:MM:SS.mmm`. The model copies the visible clock readings for the first and last action in each event. The app adds two seconds of context on either side, within the recording bounds. Previews and exports use the original recording without this clock. Rescan existing results to use this timing format. Gemini receives this MP4 and samples the picture at 4 fps. OpenAI receives at most 120 timestamped JPEG frames, at most 2 fps across the full timeline, plus notes from a separate audio-model request. Its audio pass listens for events and reactions that can help locate highlights. A clip without an audio stream needs no audio request.

This is a skim, not frame-accurate event detection. Very fast shots, small HUD text, kill counts, and overlapping speech may be unclear. The model is instructed to avoid inventing events and to use cautious labels when evidence is uncertain. Review the original-frame-rate preview before keeping a moment.

Time ranges are checked against the source duration. Overlapping ranges that extend past a clip boundary are trimmed to its bounds; reversed ranges and ranges outside the recording are omitted. The scan note reports adjustments and omissions. No extra API call is made to repair them. You can edit all valid ranges before exporting.

Recordings over five minutes are rejected before upload by default. Raise the maximum duration in settings or `--max-seconds`, subject to the media limits: 14 MB for the prepared video, 20 MB total OpenAI images, and 16 MB OpenAI WAV audio. Media is prepared and checked before uploading, and is not silently truncated.

Scanning sends media and context to the selected provider and can incur charges. Rescanning makes those requests again. Playback and exports are local. Opening an unscanned recording also starts an API search when Analyze on open is enabled. Original file paths are not sent to the provider. Visible player names and audible voice chat are part of the media. OpenAI requests set `store: false`, subject to [API data controls](https://developers.openai.com/api/docs/guides/your-data). Reports contain local paths and model observations; treat them as private if your recordings are private.

## CLI

```sh
hdcontext doctor --provider gemini
hdcontext analyze "recordings/replay.mp4" --provider gemini --output moments.json
hdcontext analyze "recordings/" --recursive --provider openai --output openai-moments.json
```

CLI analysis prints time ranges and short labels and saves a report for the GUI. The CLI defaults to Gemini; `--provider` or `HDCONTEXT_PROVIDER` selects the provider. `--model` overrides its `GEMINI_MODEL` or `OPENAI_MODEL` environment setting; `--audio-model` overrides `OPENAI_AUDIO_MODEL`. Reports are saved after the batch finishes, including on a provider limit error. Interruption before saving may lose unsaved results.

For legacy filename reports, `hdcontext apply report.json --index 1,3` previews source-file renames; add `--yes` to apply. This command renames whole source files and does not export moment ranges.

## Development

```sh
cargo fmt --check
cargo clippy -j 1 --all-targets -- -D warnings
cargo test -j 1 -- --test-threads=1
```

Tests use synthetic recordings, a local mock API, and a silent audio sink for embedded playback. They cover model time-range normalization, keeping selections out of model control, provider authentication, API failure handling, report compatibility, keeper persistence, safe exports, actual video/audio cuts, original frame rates, embedded decoding and seeking, and temporary preview cleanup. No paid API or key is needed for tests. Live event-detection quality still needs a representative game recording and your provider key.

The UI review and reproducible native screenshot fixture are documented in [docs/ui-review.md](docs/ui-review.md).

## Documentation

See the [documentation index](docs/README.md) for architecture, development, operations, troubleshooting, and UI review notes. [AGENTS.md](AGENTS.md) records the project instructions for coding agents.
