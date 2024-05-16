#!/bin/bash

set -euo pipefail

(
    set -x
    tic -xe alacritty,alacritty-direct -o target/terminfo extra/alacritty.info
)

mkdir -p target/man
for src in extra/man/*.scd
do
    output="target/man/$(basename "$src" .scd).gz"
    printf '+ scdoc %s > %s\n' "$src" "$output"
    scdoc < "$src" | gzip -c > "$output"
done

export RUSTFLAGS="-C target-cpu=native"

set -x
cargo build --release -p alacritty
cargo deb -p alacritty
