#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
target=${1:?usage: package.sh TARGET [VERSION]}
version=${2:-$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)}
case "$target:$(uname -s):$(uname -m)" in
  x86_64-unknown-linux-gnu:Linux:x86_64|aarch64-apple-darwin:Darwin:arm64) ;;
  *) echo 'Build natively on Linux x86_64 or macOS arm64 for the matching target' >&2; exit 1 ;;
esac
[[ "$version" =~ ^v?[0-9]+\.[0-9]+\.[0-9]+([-+][A-Za-z0-9.-]+)?$ ]] || { echo 'Invalid version' >&2; exit 1; }
crate_version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
base_version=${version#v}
base_version=${base_version%%-*}
base_version=${base_version%%+*}
[[ "$base_version" == "$crate_version" ]] || { echo 'Tag and Cargo version differ' >&2; exit 1; }
cargo build --release --locked --target "$target"
name="realtime-quality-${version}-${target}"
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
mkdir -p "$stage/$name/scripts" "$stage/$name/deploy" dist
cp "target/$target/release/realtime-quality" "$stage/$name/"
cp README.md config.example.toml wholesale.example.toml "$stage/$name/"
cp scripts/download_models.py scripts/models.json "$stage/$name/scripts/"
cp deploy/realtime-quality.service deploy/postgresql-quality-upgrade.sql "$stage/$name/deploy/"
"$stage/$name/realtime-quality" --version
if [[ "$target" == *linux* ]]; then
  ldd "$stage/$name/realtime-quality" | tee "$stage/linkage.txt"
  ! grep -q 'not found' "$stage/linkage.txt"
else
  otool -L "$stage/$name/realtime-quality"
fi
tar -C "$stage" -czf "dist/$name.tar.gz" "$name"
(cd dist && shasum -a 256 "$name.tar.gz" > "$name.tar.gz.sha256")
echo "dist/$name.tar.gz"
