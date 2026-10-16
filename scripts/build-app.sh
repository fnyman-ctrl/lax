#!/bin/sh
# Build Lax from source and put it in ~/Applications. Run with: npm run build:app
#
# A Lax you build yourself never gets macOS's "downloaded from the internet"
# check, so it opens without warnings and needs no Apple account. Needs the
# Xcode Command Line Tools, rustup and Node.js (see CONTRIBUTING.md).
set -eu
cd "$(dirname "$0")/.."

missing=""
xcode-select -p >/dev/null 2>&1 || missing="$missing\n  Xcode Command Line Tools: xcode-select --install"
command -v cargo >/dev/null 2>&1 || missing="$missing\n  Rust: https://rustup.rs (with Homebrew's rustup, add /opt/homebrew/opt/rustup/bin to your PATH)"
command -v node >/dev/null 2>&1 || missing="$missing\n  Node.js $(cat .nvmrc): https://nodejs.org"
if [ -n "$missing" ]; then
  printf "Lax can't be built yet. Install first:%b\n" "$missing"
  exit 1
fi

npm ci --ignore-scripts
npm run tauri build -- --bundles app

app="src-tauri/target/release/bundle/macos/Lax.app"
mkdir -p "$HOME/Applications"
# A Lax built earlier is replaced; its data in ~/Library/Application Support
# and ~/Lax stays.
rm -rf "$HOME/Applications/Lax.app"
# Moved, not copied: a second Lax.app left in the build folder shows up in
# Spotlight and Launchpad next to the real one.
mv "$app" "$HOME/Applications/"
echo
echo "Lax is in ~/Applications. Open it with: open ~/Applications/Lax.app"
