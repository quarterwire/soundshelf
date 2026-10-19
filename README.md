# SoundShelf

A fast, lightweight audio library for songs and sound effects. Add your folders once, and every sound in every subfolder shows up in one searchable list. Click to preview, then drag rows straight into Premiere (or any other editor, or Explorer).

## Download

Grab the latest `SoundShelf-…-windows-x64.exe` from [Releases](https://github.com/quarterwire/soundshelf/releases) and run it. It's a single 9 MB file with no installer, for Windows 10/11, 64-bit.

## Build

Double-click `build.bat`, or run:

```powershell
.\build.ps1
```

This builds `SoundShelf.exe` into the project folder and adds a Start menu shortcut, so it appears when you search "SoundShelf" in Windows. Run it again after pulling changes; it closes a running copy before replacing it. `.\build.ps1 -NoShortcut` skips the shortcut.

Plain `cargo build --release` also works, and puts the exe at `target\release\soundshelf.exe`. The icon and version info are embedded with MinGW's `windres`. Without it, the build still succeeds but the exe has no icon.

## Use

- **+ Add folder**, or drop folders onto the window. Subfolders are included, and the left tree filters by folder.
- **Type anywhere** to search names and folder paths. Use spaces for multiple words.
- **Click** or use the **Up/Down** keys to preview. Turn off "Play on select" if you only want playback on Enter or double-click.
- **Drag** one row, or a Ctrl/Shift multi-selection, into Premiere's timeline or Project panel.
- **Ctrl+C** copies the files themselves, the same as Explorer does.
- **Space** play/pause · **Left/Right** seek 5 s · **Esc** stop or clear search · **F5** rescan
- **SFX / Music** split clips at 30 seconds. If one is wrong, right-click it (or a multi-selection) and choose **Mark as SFX** or **Mark as Music**. Right-click a folder in the tree to mark everything in it. File marks beat folder marks, and folder marks beat the 30-second rule. Manual marks show highlighted in the **Kind** column and are saved in `%APPDATA%\SoundShelf\marks.txt`.

## YouTube downloads

**Download from YouTube** (top bar) takes a video or playlist link and saves the audio as `.m4a`, in one folder per playlist, inside your download folder. The download folder defaults to `<first library folder>\YouTube`, and you can change it with **Change…**. New files appear in the library when the download finishes.

- Playlists download 5 videos at a time. A 75-track playlist takes about 30 seconds.
- Videos already downloaded are skipped, so running the same playlist again only grabs new uploads.
- SoundShelf uses [yt-dlp](https://github.com/yt-dlp/yt-dlp). On first use it downloads `yt-dlp.exe` into `%APPDATA%\SoundShelf\bin` and updates it before each run. ffmpeg isn't needed.
- Only download audio you have the rights to use, such as your own uploads or royalty-free and Creative Commons music.
- If something fails, the full output of the last run is in `%APPDATA%\SoundShelf\youtube-download.log`.

## Loudness

Every file is measured in the background using integrated loudness (LUFS, ITU-R BS.1770 / EBU R128), the same method YouTube uses. Results are cached, so each file is only measured once. The selected file is measured first.

- The **LUFS** column is sortable. Files louder than YouTube's -14 LUFS show in orange, and very hot files (above -9) in red.
- The player shows the waveform (click to seek; clipped peaks show in red), the peak level, and how much gain to apply:
  - **YouTube (-14 LUFS):** the level YouTube plays everything at, so louder uploads get turned down anyway.
  - **Under voice (-28 LUFS):** a starting point for music and SFX beds that sit behind dialogue.
  - Boosts are capped so peaks stay under -1 dBFS.
- In Premiere, select the clip(s), press **G** (Audio Gain), and use **Adjust Gain by** with the number shown.

Settings live in `%APPDATA%\SoundShelf\config.txt`.

Playback supports WAV, MP3, FLAC, OGG, M4A/AAC (including audio-only MP4) and AIFF. Files with several audio tracks play the track that actually has sound. Opus, WMA, MKV and WebM files are listed and can be dragged, but they can't be previewed.

**Note:** Windows blocks drag-and-drop between an elevated (administrator) app and a normal one. Run SoundShelf and Premiere at the same privilege level.
