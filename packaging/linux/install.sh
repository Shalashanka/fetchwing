#!/bin/sh
# Fetchwing installer for Linux (per-user, no root needed).
#   ./install.sh            install to ~/.local
#   PREFIX=/usr/local sudo ./install.sh   system-wide
set -e
PREFIX="${PREFIX:-$HOME/.local}"
HERE="$(cd "$(dirname "$0")" && pwd)"
install -Dm755 "$HERE/fetchwing" "$PREFIX/bin/fetchwing"
install -Dm644 "$HERE/fetchwing.png" "$PREFIX/share/icons/hicolor/256x256/apps/fetchwing.png"
mkdir -p "$PREFIX/share/applications"
sed "s|@BIN@|$PREFIX/bin/fetchwing|" "$HERE/fetchwing.desktop" > "$PREFIX/share/applications/fetchwing.desktop"
command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database "$PREFIX/share/applications" 2>/dev/null || true
echo "Fetchwing installed to $PREFIX/bin/fetchwing"
case ":$PATH:" in *":$PREFIX/bin:"*) ;; *) echo "Note: add $PREFIX/bin to your PATH to use the 'fetchwing' command." ;; esac
command -v 7z >/dev/null 2>&1 || echo "Tip: install 7-Zip for archive extraction (apt install 7zip  /  dnf install p7zip-plugins)."
command -v ffmpeg >/dev/null 2>&1 || echo "Tip: install ffmpeg so yt-dlp can merge video+audio."
echo "Start it from your app menu, or run: fetchwing   (headless server: fetchwing --headless --bind 0.0.0.0)"
