#!/usr/bin/env bash
set -Eeuo pipefail

# Keep all release inputs here so a rootfs can be rebuilt from this file alone.
WALLIS_ROOTFS_VERSION="${WALLIS_ROOTFS_VERSION:-0.1.0}"
UBUNTU_CODENAME="${UBUNTU_CODENAME:-noble}"
UBUNTU_MIRROR="${UBUNTU_MIRROR:-http://archive.ubuntu.com/ubuntu}"
HERMES_AGENT_VERSION="v2026.8.31"
HERMES_AGENT_COMMIT="29112bef099274229cadff79cdff7bf7b99c4b77"
HERMES_INSTALLER_SHA256="85ef536d455e51ab67aa74d79272efd49fe717597dbaadfd3cca179a905f4706"
MNEMOSYNE_HERMES_VERSION="0.5.0"

case "$WALLIS_ROOTFS_VERSION" in
  ''|*[!0-9A-Za-z._-]*) echo "Invalid WALLIS_ROOTFS_VERSION: $WALLIS_ROOTFS_VERSION" >&2; exit 2 ;;
esac

if (( EUID != 0 )); then
  echo "Run as root (CI uses sudo)." >&2
  exit 1
fi
for command in apt-get mount umount tar sha256sum; do
  command -v "$command" >/dev/null || { echo "Missing host command: $command" >&2; exit 1; }
done

repo_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
output_dir="${ROOTFS_OUTPUT_DIR:-$repo_dir/dist/rootfs}"
mkdir -p "$output_dir"
work_dir="$(mktemp -d)"
rootfs="$work_dir/rootfs"
mounted=()
cleanup() {
  set +e
  for ((i=${#mounted[@]}-1; i>=0; i--)); do umount -l "${mounted[i]}" 2>/dev/null || true; done
  rm -rf "$work_dir"
}
trap cleanup EXIT

apt-get update
DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends debootstrap
command -v debootstrap >/dev/null || { echo "debootstrap installation failed" >&2; exit 1; }
debootstrap --variant=minbase --components=main,universe "$UBUNTU_CODENAME" "$rootfs" "$UBUNTU_MIRROR"

mount --bind /dev "$rootfs/dev"
mounted+=("$rootfs/dev")
mount --make-rslave "$rootfs/dev"
mount -t proc proc "$rootfs/proc"
mounted+=("$rootfs/proc")
mount -t sysfs sysfs "$rootfs/sys"
mounted+=("$rootfs/sys")
rm -f "$rootfs/etc/resolv.conf"
cp -L /etc/resolv.conf "$rootfs/etc/resolv.conf"

chroot "$rootfs" /usr/bin/env \
  DEBIAN_FRONTEND=noninteractive \
  APT_LISTCHANGES_FRONTEND=none \
  apt-get update
chroot "$rootfs" /usr/bin/env \
  DEBIAN_FRONTEND=noninteractive \
  APT_LISTCHANGES_FRONTEND=none \
  apt-get install -y --no-install-recommends \
    bash ca-certificates curl git passwd python3 python3-dev python3-pip \
    python3-venv build-essential libffi-dev libssl-dev procps

chroot "$rootfs" useradd --create-home --shell /bin/bash wallis
cat > "$rootfs/etc/wsl.conf" <<'EOF'
[automount]
enabled=false

[interop]
enabled=false
appendWindowsPath=false

[user]
default=wallis
EOF
cat > "$rootfs/etc/wallis-rootfs-release" <<EOF
WALLIS_ROOTFS_VERSION=$WALLIS_ROOTFS_VERSION
UBUNTU_CODENAME=$UBUNTU_CODENAME
HERMES_AGENT_VERSION=$HERMES_AGENT_VERSION
HERMES_AGENT_COMMIT=$HERMES_AGENT_COMMIT
HERMES_INSTALLER_SHA256=$HERMES_INSTALLER_SHA256
MNEMOSYNE_HERMES_VERSION=$MNEMOSYNE_HERMES_VERSION
EOF
chmod 0644 "$rootfs/etc/wsl.conf" "$rootfs/etc/wallis-rootfs-release"

installer="https://raw.githubusercontent.com/NousResearch/hermes-agent/$HERMES_AGENT_COMMIT/scripts/install.sh"
curl -fsSL "$installer" -o "$rootfs/tmp/hermes-install.sh"
printf '%s  %s\n' "$HERMES_INSTALLER_SHA256" "$rootfs/tmp/hermes-install.sh" | sha256sum -c -
chmod 0755 "$rootfs/tmp/hermes-install.sh"
chroot "$rootfs" chown wallis:wallis /tmp/hermes-install.sh
chroot "$rootfs" su -s /bin/bash - wallis -c \
  "/bin/bash /tmp/hermes-install.sh --branch '$HERMES_AGENT_VERSION' --commit '$HERMES_AGENT_COMMIT' --skip-setup --skip-browser --skip-computer-use --no-skills --non-interactive"

hermes_bin=/home/wallis/.local/bin/hermes
hermes_uv=/home/wallis/.hermes/bin/uv
hermes_python=/home/wallis/.hermes/hermes-agent/venv/bin/python
chroot "$rootfs" test -x "$hermes_bin"
chroot "$rootfs" test -x "$hermes_uv"
chroot "$rootfs" test -x "$hermes_python"
chroot "$rootfs" su -s /bin/bash - wallis -c \
  "PATH=/home/wallis/.hermes/hermes-agent/venv/bin:/home/wallis/.local/bin:\$PATH \"$hermes_uv\" pip install --python \"$hermes_python\" --no-cache-dir \"mnemosyne-hermes==$MNEMOSYNE_HERMES_VERSION\" && \"$hermes_bin\" config set memory.provider mnemosyne && \"$hermes_bin\" config set memory.memory_enabled false && \"$hermes_bin\" config set user_profile_enabled false"
chroot "$rootfs" su -s /bin/bash - wallis -c \
  "PATH=/home/wallis/.hermes/hermes-agent/venv/bin:/home/wallis/.local/bin:\$PATH \"$hermes_bin\" --version >/dev/null && \"$hermes_bin\" config get memory.provider | grep -Fx mnemosyne && \"$hermes_bin\" config get memory.memory_enabled | grep -Eiq '^(false|0)$' && \"$hermes_bin\" config get user_profile_enabled | grep -Eiq '^(false|0)$' && \"$hermes_bin\" memory status"

chroot "$rootfs" chown -R wallis:wallis /home/wallis
rm -rf "$rootfs/var/lib/apt/lists/"* "$rootfs/tmp/"*

for ((i=${#mounted[@]}-1; i>=0; i--)); do
  umount "${mounted[i]}" || { echo "Unable to unmount ${mounted[i]} before archiving" >&2; exit 1; }
done
mounted=()

archive="$output_dir/wallis-rootfs-$WALLIS_ROOTFS_VERSION.tar"
checksum="$archive.sha256"
tar --numeric-owner --sort=name --mtime=@0 -C "$rootfs" -cf "$archive" .
sha256sum "$archive" > "$checksum"
echo "$archive"
echo "$checksum"
