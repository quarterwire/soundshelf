# SoundShelf

A fast, lightweight audio library for songs and sound effects. Add your folders once, and every sound in every subfolder shows up in one searchable list. Click to preview, then drag rows straight into Premiere (or any other editor, or Explorer).

## Build

```
cargo build --release
```

The binary is at `target\release\soundshelf.exe`, a single 8 MB file with no installer.

## Use

- **+ Add folder**, or drop folders onto the window. Subfolders are included, and the left tree filters by folder.
- **Type anywhere** to search names and folder paths. Use spaces for multiple words.
- **Click** or use the **Up/Down** keys to preview. Turn off "Play on select" if you only want playback on Enter or double-click.
- **Drag** one row, or a Ctrl/Shift multi-selection, into Premiere's timeline or Project panel.
- **Ctrl+C** copies the files themselves, the same as Explorer does.
- **Space** play/pause · **Left/Right** seek 5 s · **Esc** stop or clear search · **F5** rescan
- **SFX / Music** split clips at 30 seconds.

Settings live in `%APPDATA%\SoundShelf\config.txt`.

Playback supports WAV, MP3, FLAC, OGG, M4A/AAC and AIFF. Opus and WMA files are listed and can be dragged, but they can't be previewed.

**Note:** Windows blocks drag-and-drop between an elevated (administrator) app and a normal one. Run SoundShelf and Premiere at the same privilege level.
