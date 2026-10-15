#!/bin/bash
# Host: sudo scripts/rootfs/build.sh <linux-bot> <output-dir> <version>
set -euo pipefail
bot=$(realpath "${1:?Linux bot binary required}")
output=$(realpath -m "${2:?Output directory required}")
version=${3:?Version required}
repo=$(cd "$(dirname "$0")/../.." && pwd)
mkdir -p "$output"
root=$(mktemp -d "$output/rootfs.XXXXXX")
mounted=()
cleanup() {
  local parked=true
  if mountpoint -q "$root/var/lib/wallis/workspace"; then umount "$root/var/lib/wallis/workspace" || parked=false; fi
  for ((i=${#mounted[@]}-1;i>=0;i--)); do umount -R "${mounted[i]}" || parked=false; done
  if $parked; then rm -rf "$root"; else echo "Mounted filesystems remain at $root; refusing cleanup." >&2; fi
}
trap cleanup EXIT
curl --fail --location --retry 3 https://releases.ubuntu.com/26.04/ubuntu-26.04-wsl-amd64.wsl -o "$output/ubuntu.wsl"
printf '%s  %s\n' 96c7f5fb28a7fe28245331f9bfbe4375f18dd29a4850116ad3c4f60f6700c55c "$output/ubuntu.wsl" | sha256sum -c -
tar -xf "$output/ubuntu.wsl" -C "$root"
chmod 0755 "$root"
mkdir -p "$root/opt/wallis/bin" "$root/opt/wallis/source"
install -m 0755 "$bot" "$root/opt/wallis/bin/wallis-bot"
tar -C "$repo" --exclude=.git --exclude=node_modules --exclude=target --exclude=dist --exclude=.turbo -cf - mods skills scripts/rootfs crates/wallis-core/py/speech_check.py | tar -xf - -C "$root/opt/wallis/source"
rm -f "$root/etc/resolv.conf"
cp -L /etc/resolv.conf "$root/etc/resolv.conf"
for fs in proc sys dev; do mount --rbind "/$fs" "$root/$fs"; mounted+=("$root/$fs"); mount --make-rslave "$root/$fs"; done
chroot "$root" /usr/bin/env WALLIS_VERSION="$version" /bin/bash /opt/wallis/source/scripts/rootfs/provision.sh
# Creating user namespaces inside chroot is forbidden by Linux. Give the sandbox
# checks a real mount-namespace root, as they have in WSL, without weakening them.
unshare --mount --propagation private /bin/bash -euc '
  mount --rbind "$1" "$1"
  cd "$1"
  mkdir .oldroot
  pivot_root . .oldroot
  cd /
  umount -l /.oldroot
  rmdir /.oldroot
  /opt/wallis/bin/wallis-bot --self-check
  sudo -n -u wallis -- /opt/wallis/tools/venv/bin/python /opt/wallis/source/crates/wallis-core/py/speech_check.py /var/lib/wallis
  umount /var/lib/wallis/workspace
' -- "$root"
rm -rf "$root/var/lib/wallis"
install -d -o 1000 -g 1000 -m 0755 "$root/var/lib/wallis"
rm -rf "$root/opt/wallis/source"
for ((i=${#mounted[@]}-1;i>=0;i--)); do umount -R "${mounted[i]}"; done
mounted=()
tar --numeric-owner -C "$root" -czf "$output/wallis-rootfs.tar.gz" .
(cd "$output" && sha256sum wallis-rootfs.tar.gz > wallis-rootfs.tar.gz.sha256)
