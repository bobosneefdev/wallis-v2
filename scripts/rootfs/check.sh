#!/usr/bin/env bash
set -Eeuo pipefail

archive="${1:?usage: check.sh wallis-rootfs-VERSION.tar}"
test -f "$archive"
tar -tf "$archive" | grep -Fx './etc/wsl.conf' >/dev/null
tar -tf "$archive" | grep -Fx './etc/wallis-rootfs-release' >/dev/null
tar -tf "$archive" | grep -Fx './etc/passwd' >/dev/null
tar -tf "$archive" | grep -Fx './home/wallis/.local/bin/hermes' >/dev/null
tar -tf "$archive" | grep -Fx './home/wallis/.hermes/config.yaml' >/dev/null

release="$(tar -xOf "$archive" ./etc/wallis-rootfs-release)"
printf '%s\n' "$release" | grep -Eq '^WALLIS_ROOTFS_VERSION=[0-9A-Za-z._-]+$'
printf '%s\n' "$release" | grep -Fx 'HERMES_AGENT_VERSION=v2026.8.31' >/dev/null
printf '%s\n' "$release" | grep -Fx 'HERMES_AGENT_COMMIT=29112bef099274229cadff79cdff7bf7b99c4b77' >/dev/null
printf '%s\n' "$release" | grep -Fx 'HERMES_INSTALLER_SHA256=85ef536d455e51ab67aa74d79272efd49fe717597dbaadfd3cca179a905f4706' >/dev/null
printf '%s\n' "$release" | grep -Fx 'MNEMOSYNE_HERMES_VERSION=0.5.0' >/dev/null

wsl_conf="$(tar -xOf "$archive" ./etc/wsl.conf)"
printf '%s\n' "$wsl_conf" | grep -Fx 'enabled=false' >/dev/null
printf '%s\n' "$wsl_conf" | grep -Fx 'appendWindowsPath=false' >/dev/null
printf '%s\n' "$wsl_conf" | grep -Fx 'default=wallis' >/dev/null

passwd="$(tar -xOf "$archive" ./etc/passwd)"
printf '%s\n' "$passwd" | grep -Eq '^wallis:x:[0-9]+:[0-9]+:'

config="$(tar -xOf "$archive" ./home/wallis/.hermes/config.yaml)"
printf '%s\n' "$config" | grep -Eq '^[[:space:]]*provider:[[:space:]]*mnemosyne[[:space:]]*$'
printf '%s\n' "$config" | grep -Eq '^[[:space:]]*memory_enabled:[[:space:]]*(false|0)[[:space:]]*$'
printf '%s\n' "$config" | grep -Eq '^[[:space:]]*user_profile_enabled:[[:space:]]*(false|0)[[:space:]]*$'

echo "rootfs archive checks passed: $archive"
