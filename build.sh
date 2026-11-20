#!/bin/sh
# Build a single, statically linked Linux binary.
#
# The Alpine/musl Rust toolchain needs crt-static on the final crate only
# (setting it globally breaks proc-macro builds, hence `cargo rustc`).
set -eu

cargo rustc --release -- -C target-feature=+crt-static

mkdir -p dist
cp target/release/watermarker dist/watermarker

if readelf -d dist/watermarker 2>/dev/null | grep -q NEEDED; then
    echo "WARNING: dist/watermarker still has dynamic library dependencies" >&2
    exit 1
fi

echo "Built dist/watermarker (static)"
ls -lh dist/watermarker
