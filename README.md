# Wallis V2

> A local-first Discord bot that remembers.

Wallis is a Windows desktop app that provisions and manages an official Discord bot backed by [Hermes Agent](https://github.com/NousResearch/hermes-agent). The first release targets Windows; Linux and macOS can follow once the host integration is stable.

## What is Wallis?

Wallis runs Hermes inside a dedicated [WSL2](https://learn.microsoft.com/en-us/windows/wsl/) distribution and gives it a persistent local memory layer through [Mnemosyne](https://github.com/mnemosyne-oss/mnemosyne). On first setup, the user connects a Discord bot and initializes a fictional persona whose first name is always `Wallis`.

Configuration is GUI-first: users should not need to create `.env` files, edit YAML, or change source constants. Every supported option must be available in the program and persisted by Wallis.

Wallis uses a real Discord bot account. It does not automate a normal user account or silently add itself to a server.

## Architecture

```text
Tauri desktop app
  ├── Windows UI
  ├── WSL2 provisioning and lifecycle
  └── local configuration and secret handling
        │
        ▼
  WSL2 distribution
  ├── Wallis Discord bot (voice: whisper.cpp -> Hermes -> Piper)
  ├── Hermes Agent
  └── Mnemosyne + SQLite memory
        │
        ▼
  Discord Gateway and API
```

- **Tauri 2 + Rust** — desktop shell, Windows integration, and WSL lifecycle management.
- **WSL2 runtime** — runs the Linux agent environment from a custom imported distro image. Wallis disables Windows-drive automounting and Windows-process interop, runs Hermes as a non-root user, keeps data in the Linux filesystem, and exposes no inbound service ports. WSL2 is the default contained runtime, not a hostile-code security boundary.
- **Wallis Discord bot** — owns the Discord connection and the voice loop, carried over from V1: `/join` puts Wallis in a voice channel, whisper.cpp transcribes locally, Hermes decides what to say, Piper speaks it. Hermes' own Discord gateway stays off; only one process may hold the bot token.
  Every stage stays warm: Whisper transcribes rolling snapshots while the user speaks, and Piper streams the reply from its first substantial clause. Cold-starting `hermes -z` per utterance cost about eleven seconds a turn before the model thought at all; a warm ACP session answers in about a third of a second. Speech settings (model, threads, voice, speed, silence window) are GUI options, as every user-configurable option must be.
- **Hermes Agent** — agent runtime, tool use, sessions, and scheduling, called headlessly by the bot. Wallis reads the provider/model picker from the installed Hermes instance, including providers authenticated through its CLI.
- **Mnemosyne** — local-first memory provider backed by SQLite. Keep Hermes' built-in memory disabled when Mnemosyne is the active provider to avoid duplicate memory systems.
- **Discord** — official bot account using the Gateway/API with the smallest practical permission set.

### Discord conversation settings

In **Settings → Discord**, enter comma-separated channel IDs for reading and writing. Empty lists disable text access; a channel needs both permissions for Wallis to reply there. The existing user allow-list applies to text and voice. Enable **Message Content Intent** on the bot's Discord Developer Portal page and grant **View Channel**, **Send Messages** and **Attach Files** in the selected channels. Thread destinations also need **Send Messages in Threads**. Restart Wallis after saving.

Choose a default text channel for voice requests from the write list, in the same server as the call. A request such as “Wallis, write Snake in one HTML file” produces an attachment there, followed by a spoken confirmation after the upload succeeds. Code never enters the speech pipeline as raw model output. Voice prompts explicitly account for STT homophones; text prompts identify typed input. Voice turns also receive a small recent excerpt from configured read channels in the same server, marked as typed chat. Read-only channels can supply that context without receiving replies. Grant **Read Message History** to recognize replies to Wallis after a restart.

**Natural** participation lets Hermes decide whether to contribute or stay quiet, with a configurable cooldown (20 seconds by default). Direct mentions bypass it, and voice follow-ups from the person Wallis just answered remain eligible for 20 seconds. **Only mentions** restores explicit addressing. Wallis waits for a gap before starting audio and discards stale unprompted voice replies. Per-channel ACP sessions keep simultaneous text and voice responses separate; no extra classification model or polling of channel history is involved.

Bot regression checks: `bun install --cwd src-tauri/bot --no-save`, then `bun test src-tauri/bot/bot.test.mjs`. These cover streaming audio, channel routing, attachment delivery failures, participation and simultaneous ACP replies without sending messages to Discord.

### Realtime and background models

In **Settings → AI models**, select **Realtime** or **Background** to save that role's provider, model, reasoning effort and credentials. Realtime handles conversation; when a background model is configured, it hands coding, research and substantial work to a separate warm Hermes process. Wallis acknowledges the handoff immediately, stays available for conversation, then posts the completed result in the selected allowed text channel. Voice requests receive a spoken delivery confirmation after the upload succeeds. Leave the background model blank to disable handoff. Restart Wallis after saving.

Both roles have independent effort settings, even when they use the same model. Hermes supports `agent.reasoning_overrides`, but the installed ACP adapter does not pass reasoning settings into new agents. Wallis's small process-local ACP launcher supplies the chosen effort through Hermes's native `reasoning_config` constructor argument, including when ACP rebuilds an agent for a model change. It does not modify the Hermes installation. Provider/model support still determines the available effort levels.

Background work has one active slot and three waiting slots, a ten-minute model timeout and a 512 KB output limit. Failures are reported in chat and the app. Pending jobs do not survive a restart. A blank background API key retains that provider's stored key, or copies the realtime key when both providers match; keys are stored separately with DPAPI. Providers using Hermes CLI authentication keep using that authentication.

The bot tests cover nonblocking handoff, bounded queue recovery and delivery. Run `test_hermes_acp.py` alongside `hermes_acp.py` using the installed Hermes virtualenv to check effort propagation through the actual ACP session factory without making model requests.

## Development tooling

- **TypeScript** — UI and any TypeScript-side orchestration.
- **Bun 1.4.x** — JavaScript/TypeScript runtime, package manager, standard library, and test runner. Pin the exact patch version in the root package manifest once the workspace is scaffolded.
- **Bun workspaces** — monorepo dependency management through the root `package.json` and a single `bun.lock`.
- **[Turborepo](https://turborepo.dev/)** — monorepo task graphing, caching, and orchestration. It runs package scripts; Bun remains responsible for installing and linking dependencies.
- **`tsc --noEmit`** — type checking.
- **[Oxlint](https://oxc.rs/docs/guide/usage/linter)** — JavaScript/TypeScript linting.
- **[anti-slop](https://github.com/dmmulroy/anti-slop)** — vendored, reviewed Oxlint rules; it is a repository-owned config, not a runtime dependency.
- **Effect v4 RC** — required for all TypeScript back-end runtime logic, including typed errors, cancellation, structured concurrency, and orchestration. It is not required for the frontend or tests. Rust and Hermes' Python runtime remain native to their respective layers.

No additional database, message broker, Docker Desktop installation, or container runtime is planned for the first release. If genuinely untrusted code execution becomes a requirement, add an optional Hyper-V Generation 2 Linux VM rather than weakening the default WSL2 path.

## First-release constraints

- Require Windows 10 version 2004/build 19041 or later, or Windows 11, with WSL2 and hardware virtualization available.
- Check for WSL2 before setup. If it is missing, explain the required Windows setup and reboot instead of silently changing system features.
- Expose every user-configurable option through the GUI; `.env` files, manual config-file edits, and source-constant changes are not user requirements.
- Configure the Wallis distro with `/etc/wsl.conf` settings that disable drive automounting and Windows interop; do not mount host folders into the agent runtime.
- Treat the root filesystem archive as a versioned release artifact. Do not commit `rootfs.tar` to the Git repository; verify its checksum before importing it.
- Store the Discord token and the inference provider API key using Windows Credential Manager or DPAPI. Never put them in source, logs, or an unencrypted config file; hand them to Hermes over stdin.
- The bot's prerequisites (Node, whisper.cpp, Piper, and the two local models) are downloaded into the distro during setup, checksum-verified against pinned versions. The Windows installer cannot do this: the distro does not exist until the app imports it.
- Onboarding order is fixed: import the distro, choose provider/model/reasoning effort, install Mnemosyne if it is missing, generate Wallis' persona into memory, then show his profile. The persona's first name is always `Wallis` and is never user-editable.
- Keep Mnemosyne data inside the WSL distribution and make reset/uninstall separate from stop. `wsl --unregister` permanently deletes the distribution and its memory. The Windows uninstaller unregisters the distro through an NSIS hook, but only when the user ticks "delete application data".
- Prefer mentions, DMs, and slash commands where possible. Reading arbitrary channel message content may require Discord's privileged `MESSAGE_CONTENT` intent.

## Initial implementation slice

1. Scaffold the Bun workspace and Turborepo task graph.
2. Detect WSL2 and report actionable setup errors.
3. Import and version the Wallis distro.
4. Start, stop, and inspect the Hermes process from Tauri.
5. Configure the Discord bot and persist its secret securely.
6. Verify that Mnemosyne is active and that memory survives an app restart.

## Upstream references

- [Tauri Windows prerequisites](https://v2.tauri.app/start/prerequisites/)
- [Tauri Windows installers](https://v2.tauri.app/distribute/windows-installer/)
- [Bun workspaces](https://bun.sh/docs/pm/workspaces)
- [Bun test runner](https://bun.sh/docs/test)
- [Turborepo installation and Bun support](https://turborepo.dev/docs/getting-started/installation)
- [Microsoft: import a custom WSL distribution](https://learn.microsoft.com/en-us/windows/wsl/use-custom-distro)
- [Hermes Discord integration](https://github.com/NousResearch/hermes-agent/blob/main/website/docs/user-guide/messaging/discord.md)
- [Mnemosyne Hermes integration](https://github.com/mnemosyne-oss/mnemosyne/blob/main/integrations/hermes/README.md)
