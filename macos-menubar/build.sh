#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
app_dir="$script_dir/build/VibeVM.app"
macos_dir="$app_dir/Contents/MacOS"
resources_dir="$app_dir/Contents/Resources"
config_file="$script_dir/VibeMenuBarConfig.local.swift"

if [[ ! -f "$config_file" ]]; then
  echo "Missing $config_file. Copy VibeMenuBarConfig.example.swift and set executablePath." >&2
  exit 1
fi

mkdir -p "$macos_dir" "$resources_dir"
xcrun --sdk macosx swiftc \
  -framework AppKit \
  -framework ServiceManagement \
  "$script_dir/VibeMenuBar.swift" \
  "$script_dir/VibeSettingsWindow.swift" \
  "$config_file" \
  -o "$macos_dir/VibeMenuBar"
cp "$script_dir/Info.plist" "$app_dir/Contents/Info.plist"
cp "$script_dir/icon.icns" "$resources_dir/icon.icns"
cp "$script_dir/v-menubar.png" "$resources_dir/v-menubar.png"

echo "Built: $app_dir"
