# Wallis

A Windows desktop Discord companion, built with Tauri, Svelte, Twilight and Songbird. Pi runs the realtime and worker agents; Mnemosyne provides local memory.

## Local speech

Wallis 0.3.9 uses **Parakeet TDT 0.6B v3 INT8** through Sherpa-ONNX and **Pocket TTS 3.3.0** (English September 2026). Both run on CPU. First startup downloads checksummed models and installs CPU PyTorch in the app's Python environment, including upgrades from an existing installation. No GPU or Hugging Face account is required.

Pocket audio streams directly into Songbird as 24 kHz mono float PCM; Songbird resamples it for Discord. Playback starts while the rest of the sentence is still being synthesized. Interrupting Wallis stops the queued track and closes its stream. Speaking speed adjusts playback rate and therefore pitch; 1.0 preserves the preset voice.

Alba, Marius and Javert are fixed, locally stored preset voice states. The app uses only the ungated model and accepts no audio prompts for cloning. Saved Whisper and Parakeet 110M selections migrate to Parakeet v3; Piper selections migrate to Alba at normal speed. Other preferences remain intact.

The CPU model downloads total approximately 712 MB for one preset, plus Python packages. The STT model supports 25 European languages with automatic language detection. Live Discord timing depends on CPU load and the voice connection.

## Development checks

```sh
bun install --frozen-lockfile
bun run check
bun run lint
bun run test
cargo test -p wallis-core -p wallis-discord
cargo check --workspace
```

On Windows, `cargo run -p wallis-core --example provision -- <data_dir>` checks full provisioning and a Pocket-to-Parakeet audio roundtrip. Append `--installed` to test an already provisioned directory. `python crates/wallis-core/py/speech_check.py <data_dir>` checks actual model streaming, cancellation, malformed requests and a transcription roundtrip using the installed speech environment.

Using the provisioned Python environment, `python crates/wallis-core/py/media_check.py` checks real image/GIF/MP4 decoding, frame limits, and download security without a Discord account or network requests.

The release workflow runs on pushes to `master`. Merging the 0.3.9 version bump builds and publishes a new Windows installer if `v0.3.9` does not already exist.

## Speech sources and licenses

- [NVIDIA Parakeet TDT 0.6B v3](https://huggingface.co/nvidia/parakeet-tdt-0.6b-v3): CC BY 4.0, exported and INT8 quantized by [Sherpa-ONNX](https://github.com/k2-fsa/sherpa-onnx) (Apache 2.0). Wallis uses the TDT transducer export.
- [Kyutai Pocket TTS](https://github.com/kyutai-labs/pocket-tts): MIT code and CC BY 4.0 [ungated preset-only weights](https://huggingface.co/kyutai/pocket-tts-without-voice-cloning).
- [Preset recordings](https://huggingface.co/kyutai/tts-voices): Alba MacKenna's casual voice is CC BY 4.0; Marius (Selfie) and Javert (Butter) are CC0 volunteer voice donations. Their preset states are supplied by Kyutai. Attribution is retained here; no changes are made to the source recordings.

## Discord

Wallis has one slash command, `/profile`, which shows his profile as an embed. There are no `/join` or `/leave` commands: ask him, in a text channel or in the call, and he decides. He may say no.

Text chat accepts image uploads, Discord GIF embeds, and public HTTPS media links. Wallis receives actual image inputs; GIFs and videos become timestamped frames using PyAV (installed into the existing Python environment). The realtime model must support images. Unsupported models and failed downloads produce specific media failure notes rather than silently losing attachments.

Replies select the referenced media. Phrases such as “the GIF above” select that user's latest media in the same channel. Up to 64 messages' media metadata stays in RAM for ten minutes and is removed on deletion; media bytes are not cached to disk. An upload within two minutes of a reply to that user in that channel counts as a follow-up, including in mentions-only mode. Existing user and channel access rules still apply.

Each turn examines at most four media items and twelve frames (at most six per item), resized to 768 pixels on the longest edge. Downloads require public HTTPS on port 443, validate and pin DNS addresses on every redirect, and are capped at 20 MiB per item. Decoding rejects images over 16 megapixels, samples at most the first 30 seconds of video, and does not process audio. Media processing has a shared 20 second deadline; partial samples are labelled so Wallis does not claim to have watched the whole clip. Web pages require a media embed from Discord; this is not a general web browser.

## Time

Every turn starts with the local time, and Wallis has `remind`, `cancel_reminder` and `list_reminders` tools. When he says he'll be back in ten minutes he sets a reminder with a first-person note to himself; when it falls due it arrives as a normal turn (`[reminder] …`) and he acts on it, for example by rejoining the call. Reminders live in `jobs.sqlite`, fire once, and survive restarts (an overdue one fires late and says so).
