#!/bin/bash
# Runs as root inside Ubuntu 26.04, with this repository staged at /opt/wallis/source.
set -euo pipefail
export DEBIAN_FRONTEND=noninteractive
source_dir=/opt/wallis/source
apt-get update
apt-get install -y --no-install-recommends ca-certificates curl git unzip xz-utils sudo bubblewrap ffmpeg e2fsprogs mount python3 build-essential cmake \
  libnss3 libnspr4 libatk1.0-0t64 libatk-bridge2.0-0t64 libcups2t64 libdrm2 libdbus-1-3 \
  libxkbcommon0 libx11-6 libxcomposite1 libxdamage1 libxext6 libxfixes3 libxrandr2 libgbm1 \
  libasound2t64 libpango-1.0-0 libcairo2 fonts-liberation fonts-noto-color-emoji libglib2.0-0t64 \
  glslc glslang-tools spirv-headers libvulkan-dev libvulkan1 mesa-vulkan-drivers
# Official WSL images may already have an ubuntu user at uid 1000.
if getent passwd 1000 >/dev/null && ! id wallis >/dev/null 2>&1; then
  old_user=$(getent passwd 1000 | cut -d: -f1)
  usermod -l wallis -d /home/wallis -m "$old_user"
  old_group=$(getent group 1000 | cut -d: -f1)
  groupmod -n wallis "$old_group"
fi
id wallis >/dev/null 2>&1 || useradd -m -u 1000 -s /bin/bash wallis
id worker >/dev/null 2>&1 || useradd -m -u 1001 -s /bin/bash worker
usermod -G '' wallis
usermod -G '' worker
passwd -l wallis
passwd -l worker
chmod 0700 /home/wallis
# Only the trusted bot can switch to worker. Worker cannot switch back or become root.
printf 'wallis ALL=(worker) NOPASSWD: ALL\n' > /etc/sudoers.d/wallis-worker
chmod 0440 /etc/sudoers.d/wallis-worker
visudo -cf /etc/sudoers.d/wallis-worker
cat > /etc/wsl.conf <<'CONF'
[automount]
enabled=false
[interop]
enabled=false
appendWindowsPath=false
[user]
default=wallis
CONF
# Do not keep an inherited NOPASSWD grant for uid 1000 from the base image.
rm -f /etc/sudoers.d/90-cloud-init-users
mkdir -p /opt/wallis/tools/{bun,node,clearcote,skills} /var/lib/wallis
fetch() {
  curl --fail --location --retry 3 "$1" -o /tmp/wallis-download
  printf '%s  /tmp/wallis-download\n' "$2" | sha256sum -c -
}
fetch https://github.com/oven-sh/bun/releases/download/bun-v1.4.3/bun-linux-x64-baseline.zip 1fc2edac843102909e3a1be1d8d9802cc6071cf074e67e88231f4ffe0f8b397b
unzip -oq /tmp/wallis-download -d /tmp/wallis-bun
install -m 0755 /tmp/wallis-bun/bun-linux-x64-baseline/bun /opt/wallis/tools/bun/bun
ln -sf /opt/wallis/tools/bun/bun /usr/local/bin/bun
# agent-browser's distributed native Linux CLI needs no separate Node runtime.
fetch https://github.com/yt-dlp/yt-dlp/releases/download/2026.08.19/yt-dlp_linux 58162f9bfdc27458ea47bfcb311cf47028f17d8154a8bf7d689861d46399230a
install -m 0755 /tmp/wallis-download /usr/local/bin/yt-dlp
fetch https://github.com/clearcotelabs/clearcote-browser/releases/download/v0.1.0-pre.23/clearcote-150.0.7871.114-linux-x64.tar.xz 92ebd3de6fbf2c1349d3eb3c210952fcdb8dc9ed35655a35297cd68431fc8ef2
tar -xJf /tmp/wallis-download --strip-components=1 -C /opt/wallis/tools/clearcote
printf '%s  /opt/wallis/tools/clearcote/chrome\n' cc192460484b7dcb1de8df3b3b7bea6f78962d560b6347012d9dbedd3f66720d | sha256sum -c -
cp "$source_dir/scripts/rootfs/"{package.json,bun.lock} /opt/wallis/tools/node/
(cd /opt/wallis/tools/node && bun install --frozen-lockfile --ignore-scripts)
chmod 0755 /opt/wallis/tools/node/node_modules/agent-browser/bin/agent-browser-linux-x64
ln -sf /opt/wallis/tools/node/node_modules/agent-browser/bin/agent-browser-linux-x64 /usr/local/bin/agent-browser
cp -R "$source_dir/skills/." /opt/wallis/tools/skills/
cp -R "$source_dir/mods" /opt/wallis/stock-mods
# Source tarballs omit node_modules; dependencies are root-owned and read-only to worker code.
ln -s /opt/wallis/tools/node/node_modules /opt/wallis/stock-mods/node_modules
# NeuTTS-2E's backbone runs in llama.cpp: one portable build with every x86 CPU variant plus Vulkan (NVIDIA, AMD and Intel GPUs), picked at runtime.
fetch https://github.com/ggml-org/llama.cpp/archive/dd002b91f583af0b80140f9a140dc04856bfc2e0.tar.gz 758da90e044707fc9880dfa22ad353bc4b67e2f2cb4aadfbb6e322c3a6eade2b
mkdir /tmp/llama-src && tar -xzf /tmp/wallis-download --strip-components=1 -C /tmp/llama-src
(cd /tmp/llama-src && cmake -B build -DCMAKE_BUILD_TYPE=Release -DGGML_NATIVE=OFF -DGGML_BACKEND_DL=ON -DGGML_CPU_ALL_VARIANTS=ON -DGGML_VULKAN=ON \
  -DCMAKE_BUILD_WITH_INSTALL_RPATH=ON '-DCMAKE_INSTALL_RPATH=$ORIGIN' -DLLAMA_CURL=OFF -DLLAMA_BUILD_TESTS=OFF -DLLAMA_BUILD_EXAMPLES=OFF -DLLAMA_BUILD_SERVER=ON \
  && cmake --build build --target llama-server -j"$(nproc)")
mkdir -p /opt/wallis/tools/llama
cp -a /tmp/llama-src/build/bin/. /opt/wallis/tools/llama/
cp /tmp/llama-src/LICENSE /opt/wallis/tools/llama/LICENSE
cp "$source_dir/licenses/NeuTTS-Open-License-v1.0.txt" /opt/wallis/tools/llama/NeuTTS-Open-License-v1.0.txt
/opt/wallis/tools/llama/llama-server --version
export UV_PYTHON_INSTALL_DIR=/opt/wallis/tools/python
# Mnemosyne's llama-cpp-python builds from source. Do not bake the CI host's CPU into the release.
export CMAKE_ARGS=-DGGML_NATIVE=OFF
/opt/wallis/bin/wallis-bot --provision
# Browser and compiler smoke checks are part of the image build.
/opt/wallis/tools/clearcote/chrome --headless --no-sandbox --disable-dev-shm-usage --dump-dom about:blank >/dev/null
(cd /opt/wallis/stock-mods && bun run check && bun test)
chmod -R go-w /opt/wallis
chown wallis:wallis /var/lib/wallis
printf '%s\n' "${WALLIS_VERSION:?}" > /etc/wallis-release
rm -f /tmp/wallis-download
rm -rf /tmp/wallis-bun /tmp/llama-src /root/.cache /opt/wallis/tools/downloads /var/lib/apt/lists/*
