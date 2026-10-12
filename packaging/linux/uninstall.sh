#!/bin/sh
PREFIX="${PREFIX:-$HOME/.local}"
rm -f "$PREFIX/bin/fetchwing" "$PREFIX/share/applications/fetchwing.desktop" "$PREFIX/share/icons/hicolor/256x256/apps/fetchwing.png"
echo "Fetchwing removed. Your data is kept in ~/.local/share/fetchwing (delete it manually if you want)."
