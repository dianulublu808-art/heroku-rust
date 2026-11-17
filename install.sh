#!/usr/bin/env bash
# ©️ Heroku Userbot — Rust rewrite
# Installs the toolchain, clones the repository, builds the release binary and
# starts Heroku. Safe to run again: it just updates, rebuilds and starts.
set -euo pipefail

REPO_URL="${HEROKU_REPO_URL:-https://github.com/dianulublu808-art/heroku-rust.git}"
DIR="${HEROKU_DIR:-$HOME/heroku-rust}"
MIN_MINOR=85

say() { printf '\033[1;36m[heroku]\033[0m %s\n' "$*"; }

# Termux
if command -v pkg >/dev/null 2>&1 && [ -n "${PREFIX:-}" ]; then
  say "Detected Termux, installing packages"
  pkg update -y || true
  pkg install -y git rust binutils
# Debian / Ubuntu / WSL / UserLAnd
elif command -v apt-get >/dev/null 2>&1; then
  SUDO=""
  if [ "$(id -u)" -ne 0 ]; then SUDO="sudo"; fi
  say "Detected apt, installing packages"
  $SUDO apt-get update -y
  $SUDO apt-get install -y git curl ca-certificates build-essential
# Alpine
elif command -v apk >/dev/null 2>&1; then
  say "Detected apk, installing packages"
  apk add --no-cache git curl bash build-base
else
  say "Unknown package manager: make sure git, curl and a C toolchain are installed"
fi

# The project needs Rust 1.85 or newer; distro packages are often older, so
# fall back to rustup when needed.
need_rustup=0
if ! command -v cargo >/dev/null 2>&1; then
  need_rustup=1
else
  version="$(cargo --version | awk '{print $2}')"
  minor="$(printf '%s' "$version" | awk -F. '{print $2}')"
  major="$(printf '%s' "$version" | awk -F. '{print $1}')"
  if [ "$major" -lt 1 ] || { [ "$major" -eq 1 ] && [ "$minor" -lt "$MIN_MINOR" ]; }; then
    need_rustup=1
  fi
fi

if [ "$need_rustup" -eq 1 ]; then
  say "Installing Rust 1.85+ via rustup"
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
  # shellcheck disable=SC1090
  . "$HOME/.cargo/env"
fi

if [ ! -d "$DIR/.git" ]; then
  say "Cloning $REPO_URL into $DIR"
  git clone "$REPO_URL" "$DIR"
else
  say "Updating $DIR"
  git -C "$DIR" pull --ff-only || say "Could not update, building the current tree"
fi

cd "$DIR"
say "Building release (this may take a while)"
cargo build --release
say "Starting Heroku"
exec ./target/release/heroku "$@"
