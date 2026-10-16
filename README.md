# Wallis

A Windows desktop Discord companion built with Tauri, Svelte, Twilight and Songbird. Wallis 0.5.0 runs the bot, agents, local speech, music and browser inside a private **WSL2** distro. Pi runs the realtime and worker agents; Mnemosyne provides local memory.

Install WSL2 before first setup: run `wsl --install --no-distribution` as administrator, restart Windows, then open Wallis. Virtualization must be enabled. First setup downloads a versioned, checksummed Ubuntu 26.04 runtime with CPU speech models and browser tools. It needs several GB of disk space. The Windows desktop manages settings, sign-in and updates; runtime control uses stdin/stdout without a listening port.

The Discord token remains DPAPI-encrypted on Windows and reaches the Linux bot through stdin. It is never written to Linux disk. The bot runs as `wallis`; workers and mods run as `worker` with a read-only system, private process namespace and explicit writable folders. Windows automount and executable interop are disabled. Mods cannot read either role's provider credentials; the worker can read its own provider credentials but cannot read realtime memory or the Discord token.

Existing native-install profiles, provider sign-ins, memories, jobs and reminders migrate on first setup. The original Windows data is retained. Runtime upgrades preserve Linux data and create `wallis-before-upgrade.tar` in the Windows app-data folder. Keep that recovery export until the upgrade is verified, then move it aside before the next upgrade. Uninstall asks whether to remove the distro and its data; choosing No preserves it for reinstalling.

## Mods and tools

Ask Wallis to learn a feature, for example “make a coinflip command.” His worker writes a slash-command mod in TypeScript with **Effect 4.0.5**, checks it, and registers it after the job. The kitlangton Effect skill, mod-writing guide, music guide and browser guide are available to the worker by default. The realtime agent runs commands such as `/play` and `/browse` itself and delegates feature work to the worker so conversation can continue.

Stock commands:

- `/profile` shows Wallis' profile.
- `/play query` searches for a song or uses an HTTP(S) URL. Wallis joins the requester's voice channel if needed.
- `/queue`, `/skip`, `/stop` and `/volume percent` control music.
- `/browse question` delegates web research using Clearcote and agent-browser.

Music plays independently of speech and drops to 25% of its set volume while Wallis talks. When he chooses to interrupt himself, speech stops without cutting the song. Downloads cap at 50 MiB and 20 minutes; each song is deleted after playback, and stale media is cleared at startup. Playback remains subject to the source's access restrictions, terms and applicable rights.

Anyone on the configured Discord allow-list can request features or use commands in allowed output channels. The desktop **Commands** page lists mods and lets the owner disable or delete them. Deleted stock commands stay deleted. A disabled/deleted command cannot be run through the worker. Discord may take time to propagate global command changes.

The shared `mods`, `media`, `worker-scratch` and worker profile folders use a **fixed 3 GiB filesystem**, including dependencies and browser data. Individual files cap at 50 MiB, and processes time out after five minutes. Installed runtimes and speech models are outside this workspace allowance. The worker must remove temporary downloads and browser caches. A full workspace refuses new jobs until space is freed.

Mod format: `mods/<name>/{mod.json,index.ts}`. See [the mod guide](skills/wallis-mods/SKILL.md) and [the shared Effect library](mods/_lib/wallis.ts). Every mod action uses the same Rust host dispatcher as agent tools; media paths must resolve inside `media/`.

## Local speech

Parakeet TDT 0.6B v3 INT8 runs through Sherpa-ONNX; Pocket TTS 3.3.0 uses English September 2026 preset-only weights. Both run on CPU with pinned CPU PyTorch. No GPU is required, and no Hugging Face account unless you enable voice cloning (below). Speech models total approximately 712 MB for one preset, plus Python packages. The rootfs includes Alba, Marius and Javert.

Pocket streams 24 kHz mono float PCM directly into Songbird. Speaking speed adjusts playback rate and pitch; 1.0 preserves the voice. Saved Whisper, Parakeet 110M and Piper settings migrate to the current defaults. STT supports 25 European languages with automatic detection. New transcriptions reach Wallis while he responds without stopping playback. He normally finishes his thought, but can choose `stop_speaking` to stop current and queued speech before saying something else.

## Self-tuning voice and cloning

Wallis adjusts his own delivery live, with no restart: the `tune` tool changes speed, voice, temperature, end-of-speech threshold, how eager he is to chime in (`cooldownSecs`, `followUpSecs`), pause length before he answers (`silenceMs`) and how easily he can be interrupted (`bargeInMs`). Changes persist in `tuning.json` and last until you change the matching setting in the desktop app. `list_voices` shows presets and clones.

Voice cloning is off until you enable it in the desktop app's Discord and voice settings. Enabling it requires accepting that you, the host, are fully responsible for ensuring every clone is consensual and lawful, and a Hugging Face token whose account has accepted the terms of [kyutai/pocket-tts](https://huggingface.co/kyutai/pocket-tts) (stored with Windows DPAPI like the Discord token). The cloning weights (checksummed) download on demand. Once enabled, Wallis clones from a clean WAV clip the worker prepares (for example from a YouTube video) with `clone_voice` and can switch to it immediately. Everything else works without a token.

## Staying in the conversation

Voice transcripts that arrive while he is thinking are merged into one turn, and queued speech is counted: when he is more than four sentences behind, `say` pushes back and he is told to `stop_speaking` and answer only what matters now.

## Realtime and worker

Wallis (realtime) is the only face people see. `delegate` takes a brief that says what to find and what to bring back; the worker never posts to Discord. Its report returns privately to Wallis as a `[job finished]` turn and he tells you in his own words. `channel_history` lets him read recent messages in a channel. Realtime uses [@smoose/pi-persona](https://pi.dev/packages/@smoose/pi-persona) for a soul and moods; the worker stays a plain coding agent without it.

## Time

Every turn starts with local time. `remind`, `cancel_reminder` and `list_reminders` store first-person notes in `jobs.sqlite`. A due reminder arrives as a normal turn (`[reminder] …`); it fires once and survives restarts. Wallis chooses whether to join or leave voice through conversation.

## Development checks

```sh
bun install --frozen-lockfile
bun run check
bun run lint
bun run test
cargo test -p wallis-core -p wallis-discord -p wallis-bot
cargo check --workspace
cargo build -p wallis-bot --release
```

The desktop requires Windows for DPAPI and WSL lifecycle checks. Linux desktop builds additionally require Tauri's GTK/WebKit development libraries. Run native Windows tests through the shared VM queue described in AGENTS.md.

Using the provisioned Python environment, `python crates/wallis-core/py/media_check.py` checks real image/GIF/MP4 decoding, frame limits, and download security without a Discord account or network requests.
Build the release image on an Ubuntu host with root privileges:

```sh
sudo scripts/rootfs/build.sh target/release/wallis-bot /path/to/output 0.5.0
```

This produces `wallis-rootfs.tar.gz.*` parts and the image checksum after sandbox, browser, mod and real speech roundtrip checks; GitHub caps one release asset at 2 GiB, so reassemble an importable image with `cat wallis-rootfs.tar.gz.[0-9]* > wallis-rootfs.tar.gz` and verify it against the checksum. The release workflow builds the Linux image first, then publishes it with the Windows installer under the matching version tag. Inside an imported test distro, run `/opt/wallis/bin/wallis-bot --self-check` as root for uid/filesystem isolation, browser navigation and a real Bun mod roundtrip. This initializes the workspace; use a disposable test distro. Stage `crates/wallis-core/py/speech_check.py` there and run it with `/opt/wallis/tools/venv/bin/python` as `wallis` to repeat the speech check.

## Sources and licenses

Text chat accepts image uploads, Discord GIF embeds, and public HTTPS media links. Wallis receives actual image inputs; GIFs and videos become timestamped frames using PyAV (installed into the existing Python environment). The realtime model must support images. Unsupported models and failed downloads produce specific media failure notes rather than silently losing attachments.

Replies select the referenced media. Phrases such as “the GIF above” select that user's latest media in the same channel. Up to 64 messages' media metadata stays in RAM for ten minutes and is removed on deletion; media bytes are not cached to disk. An upload within two minutes of a reply to that user in that channel counts as a follow-up, including in mentions-only mode. Existing user and channel access rules still apply.

Each turn examines at most four media items and twelve frames (at most six per item), resized to 768 pixels on the longest edge. Downloads require public HTTPS on port 443, validate and pin DNS addresses on every redirect, and are capped at 20 MiB per item. Decoding rejects images over 16 megapixels, samples at most the first 30 seconds of video, and does not process audio. Media processing has a shared 20 second deadline; partial samples are labelled so Wallis does not claim to have watched the whole clip. Web pages require a media embed from Discord; this is not a general web browser.
- [Pi](https://github.com/earendil-works/pi): MIT; agent runtime 1.2.0.
- [Bun](https://github.com/oven-sh/bun): MIT; runtime 1.4.3.
- [Effect](https://github.com/Effect-TS/effect): MIT; 4.0.5.
- [kitlangton Effect skill](https://github.com/kitlangton/skills/tree/71195e36024ebd2e66c5beac946e81eafd5abc43/skills/effect): MIT; pinned source and license in `skills/effect`.
- [yt-dlp](https://github.com/yt-dlp/yt-dlp): Unlicense source; packaged executable includes third-party components and licenses. [FFmpeg](https://ffmpeg.org/legal.html) is installed from Ubuntu.
- [Clearcote](https://github.com/clearcotelabs/clearcote-browser): BSD-3-Clause and Chromium third-party notices; pinned open build 150.0.7871.114.
- [agent-browser](https://github.com/vercel-labs/agent-browser): Apache-2.0; 0.39.1.
- [NVIDIA Parakeet TDT 0.6B v3](https://huggingface.co/nvidia/parakeet-tdt-0.6b-v3): CC BY 4.0; Sherpa-ONNX export/quantization: Apache 2.0.
- [Kyutai Pocket TTS](https://github.com/kyutai-labs/pocket-tts): MIT code, CC BY 4.0 [ungated preset-only weights](https://huggingface.co/kyutai/pocket-tts-without-voice-cloning).
- [Preset recordings](https://huggingface.co/kyutai/tts-voices): Alba MacKenna's casual voice is CC BY 4.0. Marius (Selfie) and Javert (Butter) are CC0 volunteer donations. Kyutai supplies preset states; source recordings are unchanged.
