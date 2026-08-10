#!/bin/bash
# Wrapper script to automatically recompile and run mtrace
cd "$(dirname "$0")"
cargo build --release -q
exec ./target/release/mtrace "$@"
