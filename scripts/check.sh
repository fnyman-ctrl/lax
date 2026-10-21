#!/bin/sh
# Checks that this Mac has what building and running Lax needs, before a long
# build. Run with: npm run check
#
# It only looks: nothing is installed or changed. The only network request
# is to a model server on this Mac (localhost), never anywhere else.
set -u
cd "$(dirname "$0")/.."

problems=0
ok() { printf "  ✓ %s\n" "$1"; }
bad() { printf "  ✗ %s\n    %s\n" "$1" "$2"; problems=$((problems + 1)); }
note() { printf "  ! %s\n    %s\n" "$1" "$2"; }

# "1.90.0" at least "1.90"? Compares major and minor numbers.
at_least() {
  have_major=$(echo "$1" | cut -d. -f1); have_minor=$(echo "$1" | cut -d. -f2)
  need_major=$(echo "$2" | cut -d. -f1); need_minor=$(echo "$2" | cut -d. -f2)
  [ "$have_major" -gt "$need_major" ] || { [ "$have_major" -eq "$need_major" ] && [ "${have_minor:-0}" -ge "${need_minor:-0}" ]; }
}

rust_needed=$(sed -n 's/^rust-version = "\(.*\)"/\1/p' src-tauri/Cargo.toml)
node_needed=$(tr -d 'v \n' < .nvmrc)

echo "Building Lax"
macos=$(sw_vers -productVersion 2>/dev/null || echo "?")
ok "macOS $macos on $(uname -m)"

if xcode-select -p >/dev/null 2>&1; then
  ok "Xcode Command Line Tools"
else
  bad "Xcode Command Line Tools are missing" "Install them: xcode-select --install"
fi

if command -v rustc >/dev/null 2>&1; then
  rust=$(rustc --version | cut -d' ' -f2)
  if at_least "$rust" "$rust_needed"; then
    ok "Rust $rust"
  else
    bad "Rust $rust is too old: Lax needs $rust_needed or later" "Update it: rustup update stable"
  fi
else
  bad "Rust is missing" "Install rustup from https://rustup.rs (with Homebrew's rustup, add /opt/homebrew/opt/rustup/bin to your PATH)"
fi

if command -v node >/dev/null 2>&1; then
  node=$(node --version | tr -d v)
  if at_least "$node" "$node_needed"; then
    ok "Node.js $node"
  else
    bad "Node.js $node is too old: Lax needs $node_needed or later" "Install Node.js $node_needed from https://nodejs.org"
  fi
else
  bad "Node.js is missing" "Install Node.js $node_needed from https://nodejs.org"
fi

free_gb=$(df -k . | awk 'NR == 2 { print int($4 / 1024 / 1024) }')
if [ "$free_gb" -ge 8 ]; then
  ok "$free_gb GB free disk space"
else
  bad "Only $free_gb GB free disk space" "The build needs about 8 GB. Free some space first."
fi

echo
echo "Running Lax"
memory_gb=$(( $(sysctl -n hw.memsize) / 1024 / 1024 / 1024 ))
if [ "$memory_gb" -ge 16 ]; then
  ok "$memory_gb GB memory"
else
  note "$memory_gb GB memory" "Lax works, but choose small models: see docs/MODELS.md."
fi

# Only localhost: LM Studio's and Ollama's usual ports.
found=""
for server in "LM Studio|http://localhost:1234/v1/models" "Ollama|http://localhost:11434/v1/models"; do
  name=${server%%|*}; url=${server#*|}
  if body=$(curl -s -m 2 "$url" 2>/dev/null) && [ -n "$body" ]; then
    count=$(printf "%s" "$body" | grep -o '"id"' | wc -l | tr -d ' ')
    ok "$name answers, with $count models"
    found=yes
  fi
done
if [ -z "$found" ]; then
  note "No model server answers on this Mac" "Lax builds without one, but the journal, memory, Ask and the writing assistant need one. Install LM Studio or Ollama and start its server: see docs/MODELS.md."
fi

echo
if [ "$problems" -eq 0 ]; then
  echo "Ready. Build Lax with: npm run build:app"
else
  echo "Fix the $problems problem(s) marked ✗, then run npm run check again."
  exit 1
fi
