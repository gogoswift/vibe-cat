#!/usr/bin/env bash
# File responsibility and boundaries:
# - Provide a single command for local development with auto rebuild/restart.
# - Only orchestrate cargo-watch startup and prerequisite checks.
# - Do not install dependencies automatically and do not build release bundles.
#
# Key side effects:
# - Starts a long-running watcher process and restarts the Rust app on file changes.
#
# Key dependencies and constraints:
# - Requires `cargo` and `cargo-watch` in PATH.
# - Should be run from the repository root (the script enforces this).

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR"

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
    cat <<'EOF'
Usage:
  ./dev.sh
  ./dev.sh [cargo-watch arguments...]

Default behavior:
  cargo watch -w src -w Cargo.toml -x 'run -- cat'

Examples:
  ./dev.sh
  ./dev.sh -x 'run -- gui'
EOF
    exit 0
fi

if ! command -v cargo >/dev/null 2>&1; then
    echo "error: cargo is required but not found in PATH." >&2
    exit 1
fi

if ! cargo watch --version >/dev/null 2>&1; then
    cat >&2 <<'EOF'
error: cargo-watch is not installed.
Install it first:
  cargo install cargo-watch
EOF
    exit 1
fi

if [[ "$#" -eq 0 ]]; then
    exec cargo watch -w src -w Cargo.toml -x "run -- cat"
fi

exec cargo watch -w src -w Cargo.toml "$@"
