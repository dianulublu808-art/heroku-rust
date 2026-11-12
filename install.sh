#!/usr/bin/env bash
# Build and install the Heroku Rust userbot.
set -euo pipefail

APP_NAME="Heroku"
BIN_NAME="heroku"
INSTALL_DIR="${HEROKU_INSTALL_DIR:-$HOME/.local/bin}"
DATA_DIR="${HEROKU_DATA_ROOT:-$PWD}"

info() { printf '\033[38;5;177m%s\033[0m\n' "$1"; }
die() { printf '\033[31m%s\033[0m\n' "$1" >&2; exit 1; }

command -v cargo >/dev/null 2>&1 || die "cargo not found. Install Rust: https://rustup.rs"

info "Building $APP_NAME (release)…"
cargo build --release

mkdir -p "$INSTALL_DIR"
install -m 0755 "target/release/$BIN_NAME" "$INSTALL_DIR/$BIN_NAME"
info "Installed to $INSTALL_DIR/$BIN_NAME"

mkdir -p "$DATA_DIR"
info "Data directory: $DATA_DIR"
info "Run: HEROKU_DATA_ROOT=\"$DATA_DIR\" \"$INSTALL_DIR/$BIN_NAME\""
