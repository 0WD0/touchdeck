#!/bin/sh
set -eu

cd "$(dirname "$0")/.."

if [ -f ./config.example.env ]; then
    set -a
    . ./config.example.env
    set +a
fi

cargo run --release
