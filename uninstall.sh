#!/bin/sh
# Uninstall LAST ETCHER (removes exactly what install.sh installed).
#
#   curl -fsSL https://raw.githubusercontent.com/leoplanet/last-etcher/master/uninstall.sh | sh
set -eu

BIN="$HOME/.local/bin/last-etcher"
if [ -e "$BIN" ]; then
  rm "$BIN"
  echo "Removed $BIN"
else
  echo "$BIN not found — nothing to uninstall."
  echo "(If you installed via AUR, use: yay -R last-etcher-git)"
fi
