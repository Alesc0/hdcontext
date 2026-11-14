# Operations and troubleshooting

## Saved data

On Linux, settings are stored at `$XDG_CONFIG_HOME/hdcontext/settings.json`, falling back to `~/.config/hdcontext/settings.json`. The app uses the platform configuration directory on macOS and Windows. Cached scans live in `scans/` beside settings. Settings use atomic replacement and private Unix permissions.

Provider keys are stored separately in the desktop credential store under service `io.hdcontext.desktop`. Nonempty provider environment variables take precedence. The app does not load `.env` files. Never put credentials in settings JSON, review reports, service units, or issue reports.

A saved scan is an AI result. To retain later manual trims, event names, keeper choices, and export history, use File → Save review as. Review files contain local filenames and model observations and may be private even though they contain no API key.

Folder context is shared by manual and watched-folder scans. Saved overrides take precedence over directory-name suggestions; a deliberately empty override stays empty. Existing global context is migrated only to the previously selected folder.

## Background service on Linux

Manage automatic scans in Settings → Folders. The first watched folder can enable the systemd user service. It runs while the user is signed in and starts at login; it does not configure a machine-wide daemon, enable lingering, or prevent sleep.

Inspect its state and recent logs with:

```sh
systemctl --user status hdcontext-watch.service
journalctl --user -u hdcontext-watch.service -n 50 --no-pager
```

After rebuilding or updating the executable, restart a running watcher to load the new binary:

```sh
systemctl --user restart hdcontext-watch.service
```

Restarting can resume configured scans and provider traffic. Wait for any in-flight request to finish before restarting when possible. Restarting does not clear provider pause flags or restore quota.

The generated unit points to the executable used during setup. If you move that executable, disable and re-enable background scanning in the GUI to regenerate the unit at the new location. Merely running `enable --now` against an already-running service does not replace its process.

For a manually supervised watcher:

```sh
hdcontext watch --config /path/to/settings.json
```

The watcher uses the configured folders and stored credentials. Configure these through the GUI first. On other platforms, startup supervision must be configured separately.

## Common problems

| Symptom | What to check |
| --- | --- |
| Video will not open | Confirm FFprobe can read it and GStreamer has `playbin`, `appsink`, a container demuxer, and decoders for its codecs. Install Base/Good and libav plugins as appropriate. |
| Build fails in a native dependency | Check pkg-config, GStreamer development files, D-Bus development files, Wayland, and xkbcommon prerequisites. |
| Key is not remembered | Unlock the desktop credential store, then use Retry storage or Save settings now. The app keeps the key in memory and does not write a plaintext fallback. |
| Forgotten key returns after restart | Check for a nonempty `GEMINI_API_KEY` or `OPENAI_API_KEY` environment override. Removing the stored entry does not remove environment variables. |
| GUI key works, but `doctor` reports missing key | `doctor` checks environment variables; it does not load desktop credentials. GUI and watcher credential lookup are separate from this diagnostic. |
| HTTP 429 or provider billing error | Read the classified error. Resolve quota, billing, or the provider's retry interval before resuming. ChatGPT subscriptions do not supply OpenAI API credits. There are no automatic retries or provider fallbacks. |
| Automatic scans stopped | Check enabled folders, provider pauses, available credentials, duration limits, and the service logs. After resolving a provider limit, use Resume paused automatic scans. Explicitly rescan failed recordings. |
| New file is not scanned immediately | The source must finish copying and keep the same size and modification time for six seconds. Requests run sequentially. Temporary extensions are ignored. |
| Highlight misses the event | Watch the source, drag or resize the block, or enter precise Start/End values. A rescan makes a new provider request and is not guaranteed to improve detection. |
| Export fails on the destination filesystem | Check write access, available space, naming collisions, and hard-link support. Exports refuse to overwrite existing files. |
| Modified time was preserved but filesystem creation time changed | Linux does not preserve filesystem birth time through the app's supported API. MP4 recording date and modified time are separate from that timestamp. |
| Manual changes disappeared after reopening | Save and reopen a review. Scan caching does not automatically save later manual edits and keeper choices. |

## Export failure recovery

Keep-originals mode leaves source media in place. Replacement mode is explicitly destructive after successful output publication. Review its selected ranges before confirming.

If rendering, range validation, source verification, or output publication fails before replacement, the original remains. If removing the original fails after clips are saved, retain and inspect those clips before retrying. Existing destinations will cause a subsequent export to stop rather than overwrite them.

Replacement processes each source as a group. If a later source fails, earlier successfully replaced sources remain replaced. The saved review and library reflect completed outputs, but save the review again after changes you want to retain.

## Reporting a bug

Include the app version or commit, operating system, selected provider/model without credentials, and the action that failed. For timing problems, give one filename or a redacted substitute, the proposed interval, and the actual event interval. For playback problems, include the container and codecs when known.

Redact API keys, authorization headers, private paths, player names, and voice-chat details before sharing logs or reports. Use a synthetic recording to reproduce the issue when possible.
