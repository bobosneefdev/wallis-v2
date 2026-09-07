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
  Every stage stays warm and the reply is spoken sentence by sentence as the agent streams it. Cold-starting `hermes -z` per utterance cost about eleven seconds a turn before the model thought at all; a warm ACP session answers in about a third of a second. Speech settings (model, threads, voice, speed, silence window) are GUI options, as every user-configurable option must be.
- **Hermes Agent** — agent runtime, tool use, sessions, and scheduling, called headlessly by the bot.
- **Mnemosyne** — local-first memory provider backed by SQLite. Keep Hermes' built-in memory disabled when Mnemosyne is the active provider to avoid duplicate memory systems.
- **Discord** — official bot account using the Gateway/API with the smallest practical permission set.

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
