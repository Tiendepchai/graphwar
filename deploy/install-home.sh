#!/bin/sh
set -eu
umask 077

# This installer is specific to the confirmed Armbian host and never alters its tunnel.
[ "$(id -un)" = tien ] || { printf '%s\n' 'Run as tien on the confirmed Armbian host' >&2; exit 1; }
[ "$(uname -m)" = aarch64 ] || { printf '%s\n' 'ARM64 host required' >&2; exit 1; }
[ "$#" -eq 1 ] || { printf '%s\n' 'usage: sh install-home.sh PUBLIC_HOSTNAME' >&2; exit 64; }
source_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
target=/home/tien/graphwar
for file in update-home.py compose.home.yaml graphwar-update.service graphwar-update.timer; do
  [ -f "$source_dir/$file" ] || { printf '%s\n' "Missing installer input: $file" >&2; exit 1; }
done
command -v python3 >/dev/null
python3 -I "$source_dir/update-home.py" --self-check
docker compose version >/dev/null
sudo -n true || { printf '%s\n' 'Sudo authorization required before bootstrap; run sudo -v first' >&2; exit 1; }
# Refuse an occupied target, not an upgrade/credential overwrite mechanism.
[ ! -e "$target" ] || { printf '%s\n' "$target already exists; inspect it before updating deployment files" >&2; exit 1; }
for unit in graphwar-update.service graphwar-update.timer; do
  [ ! -e "/etc/systemd/system/$unit" ] || { printf '%s\n' "$unit already installed; inspect it first" >&2; exit 1; }
done
python3 -I - "$1" <<'PY'
import re, sys
name = sys.argv[1]
if len(name) > 253 or not re.fullmatch(r"(?=.{1,253}$)(?:[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?\.)+[a-z]{2,63}", name):
    raise SystemExit("A lowercase public DNS hostname is required")
PY
mkdir -m 700 "$target"
python3 -I - "$target/compose.env" "$1" <<'PY'
import os, secrets, sys
fd = os.open(sys.argv[1], os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
with os.fdopen(fd, "w") as f:
    f.write("DOMAIN=" + sys.argv[2] + "\nPOSTGRES_PASSWORD=" + secrets.token_hex(32) + "\n")
PY
install -m 600 "$source_dir/compose.home.yaml" "$target/compose.home.yaml"
install -m 700 "$source_dir/update-home.py" "$target/update-home.py"
python3 -I "$target/update-home.py" --self-check
# Validate interpolation privately; never print the generated credential file.
GRAPHWAR_IMAGE=ghcr.io/tiendepchai/graphwar@sha256:0000000000000000000000000000000000000000000000000000000000000000 \
  docker compose --env-file "$target/compose.env" -f "$target/compose.home.yaml" config --quiet
sudo -n install -m 644 "$source_dir/graphwar-update.service" /etc/systemd/system/graphwar-update.service
sudo -n install -m 644 "$source_dir/graphwar-update.timer" /etc/systemd/system/graphwar-update.timer
sudo -n systemctl daemon-reload
# Installed disabled: no app/DB starts until the release and hostname route are verified.
printf '%s\n' 'Installed disabled. Secrets generated privately; no GitHub token or runner installed.'
printf '%s\n' 'After verifying the release, anonymous GHCR pull and tunnel route, enable explicitly:'
printf '%s\n' 'touch /home/tien/graphwar/automatic.enabled'
printf '%s\n' 'sudo systemctl enable --now graphwar-update.timer'
