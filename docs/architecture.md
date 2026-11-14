# Architecture

HDContext is a native Rust application with an egui desktop interface, GStreamer playback, FFmpeg media preparation and exports, and HTTP clients for Gemini and OpenAI. The executable also provides analysis, diagnostics, legacy rename, and headless watcher commands.

## Module map

| Module | Responsibility |
| --- | --- |
| `main.rs` | CLI parsing and command dispatch; launches the GUI by default. |
| `desktop.rs` | Library, settings, review state, background worker events, automatic scan queue, and export confirmation. |
| `player.rs` | Embedded source playback, frame stepping, normal/editor and Viewing-mode layouts, volume, timelines, and edit history. |
| `context.rs` | Normalized folder identities, saved context overrides, watched-profile context, and folder-name inference. |
| `preferences.rs` | Versioned settings, atomic private writes, debounced worker saves, and platform credential storage. |
| `provider.rs` | Provider selection, model defaults, and environment-variable names. |
| `ai.rs` | Gemini requests, shared analysis types, highlight schema, and time-range normalization. |
| `ai/openai.rs` | Audio analysis followed by timestamped-image analysis through OpenAI. |
| `ai/errors.rs` | Redacted provider errors and classification of quota, billing, and temporary limits. |
| `media.rs` | FFprobe metadata, fingerprints, bounded analysis media, timestamped frames, and audio mixing. |
| `workflow.rs` | Video discovery and analysis of a source recording. |
| `scan_cache.rs` | Persistent results, file stamps, interprocess claims, rescan requests, and provider pauses. |
| `watch.rs` | Folder discovery, copy-settling checks, profile selection, and sequential background analysis. |
| `watch_service.rs` | Linux systemd user-unit generation and enable/disable operations. |
| `segments.rs` | Range validation, excerpt rendering, export planning, staged publication, replacement, and dates. |
| `report.rs` | Report serialization, safe filenames, source validation, and legacy no-clobber renaming. |

## Playback and UI

Opening a recording starts source playback through GStreamer's `playbin` and `appsink`. Decoded display frames are scaled to at most 1280 by 720 before being copied into an egui texture. Source hashing runs separately after playback is handed off. FFmpeg does not transcode a playback proxy before the video opens.

The normal editor allocates separate vertical regions to video and controls. It measures control content to reserve space below the video; constrained windows can scroll the controls. Viewing mode hides surrounding panels, fills the workspace, and uses a compact overlay with playback controls, volume, both timelines, and the selected event label. Its idle timer does not change the video rectangle when controls hide or return.

The recording ruler seeks across the source. Highlight blocks share a second track with the same time scale. The selected block exposes trim handles in the editor; block dragging keeps its duration. Overlapping blocks are selected by repeated clicks. Fit selection changes the visible time interval while edits and seeks remain in absolute source seconds.

Highlight undo/redo stores moment snapshots and coalesces continuous gestures. A new external analysis result invalidates stale edit history. Keyboard handling yields to text inputs, menus, and modal dialogs.

## Analysis path

1. Discover source files and read media metadata. Enforce the configured duration bound before upload.
2. Select the applicable provider/model profile and resolve folder context. Prefer the most specific saved/profile context; explicit empty saved context is valid. Infer an editable game name from directory names when no override applies.
3. Claim the source through the scan cache when applicable. Claims prevent the GUI and watcher from uploading the same source concurrently.
4. Prepare bounded analysis media with FFmpeg. The full source time axis is preserved. A visible `CLIP_TIME` clock helps the model associate events with source timestamps.
5. Send combined video/audio to Gemini, or audio plus timestamped images to OpenAI. Silent sources skip the separate OpenAI audio request.
6. Parse structured results, reject invalid output, normalize usable ranges, and add surrounding event context within source bounds. No repair API call is made.
7. Save the result or failure and deliver it to the desktop. Retain manual edits made while analysis was running.

Model observations never select keepers on the user's behalf. Provider limit errors pause automatic work and stop the remaining batch. Requests are not retried or routed to another provider automatically.

## Persistent state

| State | Location/purpose | Contains credentials? |
| --- | --- | --- |
| Preferences | `settings.json` under the platform's `hdcontext` config directory; folders, profiles, models, context, and preferences. | No |
| Provider credentials | Operating system credential store under `io.hdcontext.desktop`, or process environment overrides. | Yes, only in these stores |
| Scan cache | `scans/` beside settings; source stamps, claims, results, failures, and provider pauses. | No |
| Saved review | User-chosen JSON file; manual ranges, names, keeper choices, history, and export paths. | No |
| Exported clips | User-chosen output folder, or beside sources in replacement mode. | No |

A scan cache is not an autosaved review. Later manual edits and keeper selections require saving a review. Preferences and report readers preserve version-1 compatibility through defaults and migrations. Legacy reports remain supported.

## Folder watcher

The watcher checks configured folders between requests, waits for six seconds of unchanged file size and modification time, and analyzes one eligible source at a time. More specific folder profiles take precedence over recursive parents. It reloads settings between requests so disabled folders do not drain an old queue.

The GUI can run a local watcher. On Linux, the optional systemd user service runs the same headless command while the GUI is closed. Both use the same settings, credential lookup, context resolver, provider gates, and cache locks. Exported clips are registered as completed results so watched output folders do not immediately send them for AI analysis.

## Export transaction

Exports render H.264/AAC excerpts from the original media, including mixed source audio. Default behavior keeps originals and preserves recording dates.

Exports are grouped by source. The app validates selections and source identity, obtains the relevant lock, renders temporary outputs, verifies them, sets dates, and publishes without replacing existing destinations. Publication uses hard links on the destination filesystem. Cache registration precedes original removal.

Replacement mode removes an original only after all selected outputs for that source are saved and verified. An original-removal failure leaves the saved clips available. Completed source groups remain completed if a later group fails. This is a per-source transaction, not an all-or-nothing transaction for the entire batch.

Filesystem modified/accessed times and MP4 recording metadata are preserved when requested. Windows can also preserve filesystem creation time; Linux does not copy filesystem birth time through the supported API.

## Limits

Event detection is a skim, not exact gameplay recognition. Sparse frames, ambiguous audio, and small HUD elements can produce incorrect labels or boundaries. Users review source playback and edit ranges before exporting. Only Linux has been exercised; platform credential backends exist for macOS and Windows, but those complete desktop workflows still need validation.
