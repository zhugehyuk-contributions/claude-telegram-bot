#!/bin/bash
set -e

cd /Users/USERNAME/Dev/claude-telegram-bot-ts

# Source environment variables (optional). The Rust bot also loads `.env` by itself,
# but sourcing is useful for values you don't want in the plist.
if [ -f .env ]; then
    set -a
    source .env
    set +a
fi

# Make sure we can find `claude`, `pdftotext`, etc (some launchd environments have minimal PATH).
export PATH="/Users/USERNAME/.local/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:${PATH:-}"

# Run the Rust bot (build first: `make build-rust` or `cargo build -p ctb --release` in ./rust)
exec ./rust/target/release/ctb

