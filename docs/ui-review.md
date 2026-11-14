# UI review

Three implementation and visual review passes, completed on 2026-11-14. The target was a clear, reliable editing workflow suitable for a 9/10 usability assessment. This is a self-review with rendered evidence and interaction tests, not an independent expert rating.

## Pass 1: hierarchy and navigation

The initial UI had competing toolbar buttons, repeated branding and file controls in Settings, and filenames more prominent than highlight descriptions. Error text expanded library cards into long diagnostic panels.

Changes:

- Made event descriptions the primary library labels, with filenames and scan state below.
- Added library search and All, Highlights, and Needs attention filters.
- Grouped review file actions under File and kept the active recording's rescan inside the player.
- Split Settings into Analysis, Folders, and Storage.
- Increased text contrast and font sizes, with a consistent panel and selection palette.
- Added actionable empty-state buttons and a quiet status bar for counts and unsaved edits.

## Pass 2: timeline and editing

Timeline labels overlapped trim handles. Very short ranges had misleading handle positions. Resizing a boundary was hard to reverse, and time inputs displayed raw seconds.

Changes:

- Separated labels from the graph while keeping ruler and range positions aligned.
- Kept handles at their actual timestamps, with separate hit areas for short ranges.
- Added Fit selection and Full clip views. Zoomed seeking still uses absolute recording times.
- Added an accessible Highlights menu for navigating ranges, including ranges outside the visible list.
- Added undo/redo with one undo step per drag or typing session. New scan results reset stale undo history.
- Added reversible marker removal, precise timecode inputs, and playhead-based trimming.
- Made the selected range's export action visually primary.
- Moved volume, audio track, sizing, and shortcut help into Playback.

## Pass 3: compact layouts and recovery

At 1000 by 700 with Settings open, editing and export originally fell below the visible area. Showing an error made this worse. Merely opening a recording also created an unnecessary save prompt.

Changes:

- Adapted video height and controls to available space, with a bounded highlight list.
- Kept selection export and rescan visible at the compact size.
- Preserved the preferred video height when space temporarily shrinks.
- Replaced large diagnostic blocks with compact, actionable messages and expandable details.
- Added background setup recovery and playback retry actions.
- Stopped marking navigation and already-cached AI results as unsaved manual edits.
- Kept original recordings and exported files outside marker undo/removal operations.

## Evidence

Rendered the actual native app with synthetic video in an isolated fixture. The fixture never loads saved preferences or credentials and disables API requests. Reviewed the editor at 1440 by 960, Settings plus editor at 1000 by 700, an error state at 1200 by 800, and empty, watched-folder, and zoomed states.

Interaction tests cover timeline seeking, overlapping-range selection, both trim handles, zoomed timestamps, timecode parsing, undo/redo transactions, stale-history invalidation, native frame stepping, text-field shortcut exclusion, Ctrl+Z handling, pane resizing, compact export/rescan visibility, cached results, and background job safety. Existing media/export checks remain in the suite.

To reproduce a native screenshot using a synthetic recording of at least 180 seconds:

```sh
cargo build --example ui_review
./target/debug/examples/ui_review /path/to/synthetic.mp4 /tmp/editor.png editor 1440 960
./target/debug/examples/ui_review /path/to/synthetic.mp4 /tmp/compact.png settings 1000 700
```

Other fixture states are `empty`, `error`, `folders`, `storage`, `zoom`, `export`, `replace`, `expanded`, and `hidden`. Use a debug build. On this Wayland desktop, captures used X11 through `env -u WAYLAND_DISPLAY WINIT_UNIX_BACKEND=x11` so compositor throttling did not suspend the review window. Screenshots come from the app's framebuffer, not the user's desktop.

## Remaining validation

An independent expert rating requires external review. Real-user usability sessions, screen-reader testing, and large-library performance measurements have not been completed. Backward frame stepping in variable-frame-rate footage still follows the negotiated frame rate. The UI does not claim to make the model's event detection exact.

## Export replacement follow-up

The export dialog now defaults to keeping originals and preserving recording dates. Replacement mode previews selected ranges, explains that unselected footage is removed, and requires an explicitly labelled export-and-remove action. Each source is replaced only after all of its selected clips are rendered and verified. Linux filesystem creation dates are explicitly distinguished from preserved modified times and MP4 recording dates.

## Current player layout

Subsequent changes consolidated highlights onto one shared timeline. Repeated clicks cycle overlapping ranges; the selected name is shown separately. In the normal editor, controls remain visible below the video with no overlap. Viewing mode fills the workspace and uses an auto-hiding overlay containing basic playback, mute/volume, both timelines, and the selected name. Back to editor or Escape restores editing. Tests cover the separate normal regions, idle control visibility, overlay hiding/reveal without video movement, mode navigation, and direct volume changes.

For current screenshot commands and fixture behavior, see [Development](development.md#native-visual-review).
