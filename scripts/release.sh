#!/usr/bin/env bash
# Builds the Windows binaries and a signed update folder.
#   scripts/release.sh 0.1.0 path/to/update-signing.key out/updates
set -euo pipefail
version=${1:?version}; key=${2:?signing key file}; out=${3:?output dir}
cd "$(dirname "$0")/.."

cargo build --release --target i686-pc-windows-gnu -p kotor-client -p kotor-launcher
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
cp -r dist/. "$stage/"
rm -f "$stage/README.md"
cp target/i686-pc-windows-gnu/release/kotor_coop.dll "$stage/kotor-coop/"
cargo run --release -p kotor-launcher -- make-release --from "$stage" --out "$out" --version "$version" --key "$key"
echo "Upload $out to your update host (e.g. the master's --updates-dir)."
