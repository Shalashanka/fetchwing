#!/bin/bash
# Build all release artifacts into dist/ (run from Git Bash on Windows with zig + Inno Setup installed).
set -e
cd "$(dirname "$0")/.."
VER=$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)
cargo build --release
cargo test --release
CARGO_TARGET_DIR=target-linux cargo zigbuild --release --target x86_64-unknown-linux-gnu.2.28
mkdir -p dist
"$LOCALAPPDATA/Programs/Inno Setup 6/ISCC.exe" packaging/windows/fetchwing.iss
cp target/release/fetchwing.exe "dist/fetchwing-$VER-windows-x64-portable.exe"
L="dist/fetchwing-$VER-x86_64-linux"; rm -rf "$L"; mkdir -p "$L"
cp target-linux/x86_64-unknown-linux-gnu/release/fetchwing "$L/"
cp packaging/linux/{install.sh,uninstall.sh,fetchwing.desktop,fetchwing.service} assets/icon-256.png README.md "$L/"
mv "$L/icon-256.png" "$L/fetchwing.png"
if command -v wsl >/dev/null; then
  W=$(wsl -d Ubuntu -- wslpath -u "$(pwd -W)/dist")
  N=$(basename "$L")
  wsl -d Ubuntu -- bash -c "cd '$W' && chmod +x $N/fetchwing $N/install.sh $N/uninstall.sh && tar -czf /tmp/fw.tgz $N && cp /tmp/fw.tgz $N.tar.gz"
else
  tar -C dist -czf "$L.tar.gz" "$(basename "$L")"
fi
rm -rf "$L"
(cd dist && sha256sum * > SHA256SUMS.txt)
ls -la dist
