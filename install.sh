#!/usr/bin/env bash
# Install nevis for the current user (building it first, in a checkout; a release comes built):
#   ~/.local/bin/nevis (launcher) and nevis-app, the desktop entry, and a systemd user
#   service that keeps nevis-app running.
# Set NEVIS_DEFAULT=0 to skip making it the default app for .txt and .md files.
set -euo pipefail
cd "$(dirname "$0")"

bindir="$HOME/.local/bin"
apps="$HOME/.local/share/applications"
icons="$HOME/.local/share/icons/hicolor"
units="$HOME/.config/systemd/user"

if [[ -x bin/nevis-app ]]; then
    built=bin
else
    # Capped parallelism: an all-core build overheats this laptop.
    cargo build --release -j "${JOBS:-6}"
    built=target/release
fi

install -Dm755 "$built/nevis" "$bindir/nevis"
install -Dm755 "$built/nevis-app" "$bindir/nevis-app"
mkdir -p "$apps" "$units"
first_install=true
[[ -e "$units/nevis.service" ]] && first_install=false
sed "s|@BINDIR@|$bindir|" data/dev.shirvani.Nevis.desktop.in > "$apps/dev.shirvani.Nevis.desktop"
sed "s|@BINDIR@|$bindir|" data/nevis.service.in > "$units/nevis.service"
install -Dm644 data/dev.shirvani.Nevis.svg "$icons/scalable/apps/dev.shirvani.Nevis.svg"
update-desktop-database -q "$apps" || true
gtk-update-icon-cache -q -t "$icons" 2>/dev/null || true

if [[ "${NEVIS_DEFAULT:-1}" == 1 ]]; then
    xdg-mime default dev.shirvani.Nevis.desktop text/plain text/markdown text/x-markdown
fi

systemctl --user daemon-reload
# The service is only switched on by the first install; later installs leave it as it is.
if $first_install; then
    systemctl --user enable --now nevis.service
elif systemctl --user is-active --quiet nevis.service; then
    echo "nevis is running the previous build. To switch (this closes its windows):"
    echo "    systemctl --user restart nevis.service"
fi
echo "Installed $bindir/nevis"
