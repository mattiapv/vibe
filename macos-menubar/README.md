# Vibe

A dependency-free macOS menu bar app for the shared `vibe ssh --main` VM. It
checks `vibe ssh --list` every five seconds and can start or stop the `main` VM.

The uncommitted `VibeMenuBarConfig.local.swift` contains the absolute path of
your Vibe executable. Create it from `VibeMenuBarConfig.example.swift`, then
set `executablePath`. The app starts the main VM with
`vibe ssh --main --no-mount`, which leaves existing mounted folders unchanged.

On a Mac with the Xcode Command Line Tools installed, build and open the app:

```sh
cd macos-menubar
chmod +x build.sh
./build.sh
open "build/Vibe Menu Bar.app"
```

The app is an accessory app, so it does not appear in the Dock. It starts Vibe
without an interactive SSH terminal; Vibe's SSH supervisor continues running
after that command exits.

`icon.icns` is bundled as the VibeVM Finder icon.
`v-menubar.png` is bundled as the monochrome Vibe menu-bar icon.

Open Settings to manage the folders in `~/.cache/vibe/main/mounted-folders.txt`.
Restart Vibe after saving for mount changes to apply.

On macOS 13 or later, Settings also provides an Open Vibe at login toggle.
