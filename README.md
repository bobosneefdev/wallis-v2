# Wallis V2

> A local-first Discord companion that remembers.

Wallis is a Windows desktop app that runs a Discord bot with a voice, a memory, and a background worker. The whole backend is Rust inside the Tauri process; the UI is Svelte 5 + TypeScript. There is no WSL, container or VM.

## How it works

```
Tauri app (Rust)
├── wallis-discord   serenity + songbird: gateway, slash commands, text, voice receive/playback
├── wallis-core      Pi RPC, jobs, speech, provisioning, prompts (pure logic, tested on Linux)
├── Pi (realtime)    conversation + long-term memory (Mnemosyne over MCP); no shell or file tools
├── Pi (worker)      coding/research in a scratch folder; NO memory
├── whisper-server   native speech-to-text (whisper.cpp)
└── Piper server     native text-to-speech (embedded Python venv)
```

- **Pi agent harness** runs both agents in `--mode rpc`. The realtime agent acts only through tools forwarded by a small Pi extension (`say`, `message`, `file`, `skip`, `delegate`, plus Mnemosyne's memory tools). Side effects are tool calls; memory writes are the agent's decision.
- **Realtime knows the worker has no memory** and must pass everything it needs through `delegate`'s `context`. The worker is launched with `--no-mcp` and no Wallis tools, so memory isolation is structural, not a prompt.
- **Models are configured per role** (provider, model, reasoning level, Fast mode). Sign in with a ChatGPT Codex subscription from Settings and both dropdown sets come from your live Codex catalog; or type any Pi provider and model by hand.
- **Background jobs** live in SQLite. A job cut off by a restart is reported failed in its channel and the app, with a manual retry; it is never silently re-run.
- **Provisioning** downloads pinned, SHA-256-verified builds of Pi, whisper.cpp and uv on first start, then builds a Python 3.12 venv with Piper and Mnemosyne and fetches the chosen voice models.
- **Secrets**: the Discord token is stored with Windows DPAPI. Codex tokens live in each role's Pi `auth.json` and are refreshed by the app.

## Participation

In voice and text, **Natural** lets the realtime agent decide whether to contribute, with a cooldown (5 s default). Direct mentions bypass it, and voice follow-ups from the person Wallis just answered stay eligible for 20 s. **Only mentions** requires explicit addressing.

## Discord setup

Enter comma-separated channel IDs for reading and writing in the app. Empty lists disable text access; a channel needs both permissions for Wallis to reply there. The user allow-list applies to text and voice; `*` allows anyone. Enable **Message Content Intent** in the Discord Developer Portal and grant **View Channel**, **Send Messages** and **Attach Files** in the selected channels (plus **Send Messages in Threads** for threads). Restart Wallis after saving.

## Security model

The realtime agent has no shell or file tools. The worker runs in an AppContainer inside a Job Object (kill-on-close, process and memory caps), with write access only to its scratch folder and Pi config, outbound network allowed, and a bundled busybox-w32 shell. This is below VM-grade isolation and is not a boundary against hostile code. On Windows 11 the sandbox smoke test (scratch write allowed, outside read/write denied, Pi RPC works inside it) and full provisioning (Pi, whisper, Piper, Python venv, models, Piper to whisper round trip) pass. The cross-built Tauri app launches and renders on Windows 11 (the WebView2 loader DLL must sit beside the exe; CI ships both). Codex sign-in was checked against Pi 1.2.0's own bundled implementation (same client id, authorize parameters, token exchange, refresh and `auth.json` shape); the catalog parser reads the upstream Codex `models.json`; a live sign-in and the live catalog response remain unexercised. The Discord client reaches real Discord and is rejected only for a fake token (`cargo run -p wallis-discord --example login`); it has never logged in as a real bot. Both need live credentials.

CI (`.github/workflows/build.yml`) runs on the sneefware self-hosted runners and does one thing: on every push to `master` it cross-builds the Windows release (`wallis.exe` + `WebView2Loader.dll`) and uploads it as the `wallis-windows` artifact.

## Stack

- **Tauri 2 + Rust** (cargo workspace: `src-tauri`, `crates/wallis-core`, `crates/wallis-discord`).
- **Svelte 5 + TypeScript** UI in `apps/desktop`, built with Vite; **Bun** for packages and tests, **Oxlint**, **svelte-check**, **Turborepo**.
- Rust is tested on Linux (`cargo test -p wallis-core -p wallis-discord`); the app itself targets Windows 10 2004+ / Windows 11. Building `wallis-discord` needs `cmake` (libopus).

## Checks

`bun run check && bun run lint && bun run test && cargo test -p wallis-core -p wallis-discord && cargo check --workspace`

## Upstream references

- [Tauri Windows prerequisites](https://v2.tauri.app/start/prerequisites/)
- [Tauri Windows installers](https://v2.tauri.app/distribute/windows-installer/)
- [Pi coding agent](https://github.com/badlogic/pi-mono)
- [Mnemosyne](https://github.com/mnemosyne-oss/mnemosyne)
- [whisper.cpp](https://github.com/ggml-org/whisper.cpp), [Piper](https://github.com/OHF-Voice/piper1-gpl)
