#!/bin/sh
# Regenerate every icon from its SVG source. Run with: npm run icons
#
# src-tauri/icons/app-icon.svg       -> the app icon in every size Tauri needs
# src-tauri/icons/tray/active.svg    -> menu bar icon while capturing
# src-tauri/icons/tray/paused.svg    -> menu bar icon while paused
#
# Uses the Tauri CLI only; nothing is downloaded.
set -eu
cd "$(dirname "$0")/.."
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

tauri icon src-tauri/icons/app-icon.svg -o "$tmp/app"
# Desktop icons only; the CLI also makes Android and iOS folders we don't ship.
find "$tmp/app" -maxdepth 1 -type f -exec cp {} src-tauri/icons/ \;

for state in active paused; do
  tauri icon "src-tauri/icons/tray/$state.svg" -p 36 -o "$tmp/$state"
  cp "$tmp/$state/36x36.png" "src-tauri/icons/tray/$state.png"
done
