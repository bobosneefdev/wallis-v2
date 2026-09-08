// Wallis' Discord bot. Runs inside the Wallis WSL2 distro next to Hermes; the desktop app
// installs this file, its dependencies and the local models, then starts it with the
// credentials and voice settings in the environment. Plain ESM on purpose: no build step, so
// the desktop app can embed this file verbatim and write it into the distro.
//
// Voice loop: Discord opus -> whisper.cpp -> Hermes -> Piper -> Discord. Every stage is a warm
// long-lived process, and a reply is spoken in natural chunks as the agent streams it, so
// the first audio leaves before the last word is written.
import { spawn } from "node:child_process";
import { existsSync, readFileSync, writeFileSync } from "node:fs";
import { createInterface } from "node:readline";
import { Readable } from "node:stream";
import { join } from "node:path";
import {
  AudioPlayerStatus,
  createAudioPlayer,
  createAudioResource,
  EndBehaviorType,
  joinVoiceChannel,
  StreamType,
} from "@discordjs/voice";
import { Client, EmbedBuilder, Events, GatewayIntentBits, MessageFlags, REST, Routes, SlashCommandBuilder } from "discord.js";
import ffmpegPath from "ffmpeg-static";
import { opus } from "prism-media";

const HOME = process.env.WALLIS_HOME ?? "/home/wallis/.wallis";
const HERMES = process.env.HERMES_BIN ?? "/home/wallis/.local/bin/hermes";
const PROFILE = process.env.WALLIS_PROFILE ?? "/home/wallis/.hermes/wallis-profile.json";
const TOKEN = process.env.DISCORD_BOT_TOKEN;
const APPLICATION_ID = process.env.DISCORD_APPLICATION_ID;
const ALLOWED = new Set((process.env.DISCORD_ALLOWED_USERS ?? "").split(",").map((id) => id.trim()).filter(Boolean));
const STT_MODEL = join(HOME, "models", `ggml-${process.env.WALLIS_STT_MODEL ?? "base.en"}.bin`);
const STT_THREADS = process.env.WALLIS_STT_THREADS ?? "4";
const TTS_MODEL = join(HOME, "models", `en_US-ryan-${process.env.WALLIS_TTS_VOICE ?? "medium"}.onnx`);
const TTS_SPEED = Number(process.env.WALLIS_TTS_SPEED ?? 1);
const SILENCE_MS = Number(process.env.WALLIS_SILENCE_MS ?? 250);
const PARTIAL_MS = 750;
const STT_URL = "http://127.0.0.1:8085";
const TTS_URL = "http://127.0.0.1:8086";
const STATUS_FILE = join(HOME, "bot-status.json");
const GREETING = "Sup boys";
/** Wallis only speaks when spoken to, the way V1 did — his name is the trigger. */
const NAME = /\bwall(?:is|ace|y|ie)\b/i;
const MIN_AMPLITUDE = 500;
const VOICE_BRIEF = "You are in a Discord voice call with friends. The user's words come from speech-to-text, so interpret names and homophones by sound without commenting on their spelling. Answer out loud in one or two short spoken sentences. Plain speech only: no markdown, no emoji, no stage directions, no lists.";

process.env.FFMPEG_PATH = ffmpegPath;

const diagnostics = { lastError: null, lastLatency: null };
const log = (...parts) => console.log("[wallis]", ...parts);
const saveDiagnostics = () => { try { writeFileSync(STATUS_FILE, JSON.stringify(diagnostics)); } catch {} };
function diagnose(kind, value) {
  diagnostics[kind] = { at: new Date().toISOString(), ...value };
  saveDiagnostics();
}
function recovered(where) {
  if (diagnostics.lastError?.where === where) { diagnostics.lastError = null; saveDiagnostics(); }
}
const failed = (where, error) => {
  const message = error instanceof Error ? error.message : String(error);
  log(`${where}:`, message);
  diagnose("lastError", { where, message });
};
saveDiagnostics();
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
let connection = null;
let listening = null;
const player = createAudioPlayer();
player.on("error", (error) => failed("player", error));

/** Waits for a local service to answer, so we never fire a request into a half-started server. */
async function waitFor(url, what) {
  for (let attempt = 0; attempt < 120; attempt += 1) {
    try { await fetch(url); return; } catch { await sleep(500); }
  }
  throw new Error(`${what} did not become ready`);
}

// ------------------------------------------------------------ local speech services

let whisper = null;
let piper = null;
let speechStarting = null;

const PIPER_SERVER = String.raw`
import json, sys
from http.server import BaseHTTPRequestHandler, HTTPServer
from piper import PiperVoice, SynthesisConfig

voice = PiperVoice.load(sys.argv[1])

class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.0"

    def do_GET(self):
        self.send_response(200)
        self.end_headers()

    def do_POST(self):
        try:
            body = json.loads(self.rfile.read(int(self.headers.get("Content-Length", "0"))))
            audio = voice.synthesize(body["text"], SynthesisConfig(length_scale=body.get("length_scale")))
            self.send_response(200)
            self.send_header("Content-Type", "audio/L16;rate=22050;channels=1")
            self.end_headers()
            for chunk in audio:
                self.wfile.write(chunk.audio_int16_bytes)
                self.wfile.flush()
        except Exception as error:
            print(error, file=sys.stderr, flush=True)

    def log_message(self, *_):
        pass

HTTPServer(("127.0.0.1", int(sys.argv[2])), Handler).serve_forever()
`;

async function startSpeech() {
  speechStarting ??= (async () => {
    if (!whisper) {
      const child = spawn(join(HOME, "whisper", "whisper-server"), ["-m", STT_MODEL, "--port", "8085", "-t", STT_THREADS], { stdio: ["ignore", "ignore", "pipe"] });
      whisper = child;
      child.stderr.resume();
      child.on("exit", (code) => { if (whisper === child) { whisper = null; diagnose("lastError", { where: "whisper-server", message: `exited ${code}` }); } });
      await waitFor(`${STT_URL}/`, "whisper-server");
      recovered("whisper-server");
      log("speech recognition ready");
    }
    if (!piper) {
      const child = spawn(join(HOME, "piper", "bin", "python"), ["-u", "-c", PIPER_SERVER, TTS_MODEL, "8086"], { stdio: ["ignore", "ignore", "pipe"] });
      piper = child;
      child.stderr.on("data", (data) => diagnose("lastError", { where: "piper", message: data.toString().trim().slice(-500) }));
      child.on("exit", (code) => { if (piper === child) { piper = null; diagnose("lastError", { where: "piper", message: `exited ${code}` }); } });
      await waitFor(`${TTS_URL}/`, "piper");
      recovered("piper");
      log("voice ready");
    }
  })();
  try { await speechStarting; } finally { speechStarting = null; }
}

async function transcribe(wav, signal) {
  const body = new FormData();
  body.append("file", new Blob([wav], { type: "audio/wav" }), "audio.wav");
  body.append("response_format", "json");
  body.append("no_timestamps", "true");
  body.append("token_timestamps", "false");
  const response = await fetch(`${STT_URL}/inference`, { method: "POST", body, signal });
  if (!response.ok) return "";
  const data = await response.json();
  // Whisper marks non-speech as bracketed events; drop those rather than transcribe them.
  return (data.text ?? "").replace(/\[.*?\]/g, "").replace(/\(.*?\)/g, "").trim();
}

/** 48kHz interleaved stereo -> 16kHz mono, which is the only shape whisper.cpp accepts. */
export function downsample(pcm) {
  const input = new Int16Array(pcm.buffer, pcm.byteOffset, Math.floor(pcm.length / 2));
  const frames = 3; // 48000 / 16000
  const output = new Int16Array(Math.floor(input.length / 2 / frames));
  for (let index = 0; index < output.length; index += 1) {
    let sum = 0;
    for (let offset = 0; offset < 2 * frames; offset += 1) sum += input[index * 2 * frames + offset] ?? 0;
    output[index] = Math.round(sum / (2 * frames));
  }
  return output;
}

export function toWav(samples) {
  const bytes = samples.length * 2;
  const wav = Buffer.alloc(44 + bytes);
  wav.write("RIFF", 0);
  wav.writeUInt32LE(36 + bytes, 4);
  wav.write("WAVEfmt ", 8);
  wav.writeUInt32LE(16, 16);
  wav.writeUInt16LE(1, 20);
  wav.writeUInt16LE(1, 22);
  wav.writeUInt32LE(16000, 24);
  wav.writeUInt32LE(32000, 28);
  wav.writeUInt16LE(2, 32);
  wav.writeUInt16LE(16, 34);
  wav.write("data", 36);
  wav.writeUInt32LE(bytes, 40);
  Buffer.from(samples.buffer, samples.byteOffset, bytes).copy(wav, 44);
  return wav;
}

// ---------------------------------------------------------------- speaking

/** One spoken chunk at a time, in order: the queue is what lets us start talking early. */
const speech = { queue: [], current: null };
let synthesis = Promise.resolve();

function playNext() {
  if (speech.current) return;
  const item = speech.queue.shift();
  if (!item) return;
  speech.current = item;
  player.play(item.resource);
}

player.on(AudioPlayerStatus.Idle, () => {
  speech.current = null;
  playNext();
});

player.on(AudioPlayerStatus.Playing, () => {
  const timing = speech.current?.timing;
  if (!timing || timing.playingAt) return;
  timing.playingAt = performance.now();
  diagnostics.lastError = null;
  diagnose("lastLatency", {
    stopToTranscriptMs: Math.round(timing.transcribedAt - timing.stoppedAt),
    transcriptToFirstTextMs: Math.round(timing.firstTextAt - timing.transcribedAt),
    firstTextToAudioMs: Math.round(timing.playingAt - timing.firstTextAt),
    totalMs: Math.round(timing.playingAt - timing.stoppedAt),
  });
});

async function synthesize(text, timing) {
  await startSpeech();
  let response;
  try {
    response = await fetch(`${TTS_URL}/synthesize`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ text, length_scale: TTS_SPEED }),
    });
  } catch (error) {
    const child = piper;
    piper = null;
    child?.kill();
    throw error;
  }
  if (!response.ok || !response.body) {
    const child = piper;
    piper = null;
    child?.kill();
    throw new Error(`piper refused: ${response.status}`);
  }

  const audio = Readable.fromWeb(response.body);
  const encoded = encodeSpeech(audio);
  speech.queue.push({ resource: createAudioResource(encoded, { inputType: StreamType.OggOpus }), timing });
  playNext();
}

/** Keep the PCM pipe streaming: probing and one-second Ogg pages otherwise buffer speech. */
export function encodeSpeech(audio) {
  const encoder = spawn(ffmpegPath, [
    "-loglevel", "error", "-f", "s16le", "-ar", "22050", "-ac", "1",
    "-probesize", "32", "-analyzeduration", "0", "-i", "pipe:0",
    "-ar", "48000", "-ac", "2", "-c:a", "libopus", "-page_duration", "20000", "-flush_packets", "1", "-f", "ogg", "pipe:1",
  ], { stdio: ["pipe", "pipe", "pipe"] });
  encoder.stderr.on("data", (data) => diagnose("lastError", { where: "ffmpeg", message: data.toString().trim().slice(-500) }));
  encoder.on("error", (error) => failed("ffmpeg", error));

  audio.on("error", (error) => { encoder.stdin.destroy(); failed("piper stream", error); });
  encoder.stdin.on("error", (error) => failed("ffmpeg input", error));
  audio.pipe(encoder.stdin);
  return encoder.stdout;
}

function say(text, timing = null) {
  const spoken = text.trim();
  if (!spoken) return Promise.resolve();
  synthesis = synthesis.then(() => synthesize(spoken, timing)).catch((error) => failed("synthesis failed", error));
  return synthesis;
}

/** Strip anything that would be read aloud as punctuation soup. Applied to a finished sentence,
 *  never to a streaming buffer: trimming a chunk's edge welds its last word onto the next chunk. */
export function speakable(text) {
  return text.replace(/```[\s\S]*?```/g, " ").replace(/[*_`#>]/g, "").replace(/\s+/g, " ").trim();
}

/** Pull off complete sentences, or one substantial first clause, leaving the tail buffered. */
export function takeSentences(buffer, allowClause = false) {
  const sentences = [];
  const boundary = /[.!?…]["')\]]*\s+/g;
  let start = 0;
  if (allowClause) {
    const clause = /[,;:—–]\s+/.exec(buffer);
    const sentence = boundary.exec(buffer);
    boundary.lastIndex = 0;
    if (clause && (!sentence || clause.index < sentence.index) && buffer.slice(0, clause.index).trim().split(/\s+/).length >= 6) {
      sentences.push(buffer.slice(0, clause.index + clause[0].trim().length).trim());
      start = clause.index + clause[0].length;
      boundary.lastIndex = start;
    }
  }
  for (let match = boundary.exec(buffer); match; match = boundary.exec(buffer)) {
    sentences.push(buffer.slice(start, match.index + match[0].trim().length).trim());
    start = boundary.lastIndex;
  }
  return { sentences, rest: buffer.slice(start) };
}

/** Joins a rolling transcript to the final overlapping tail without repeating words. */
export function mergeTranscript(prefix, suffix) {
  const left = prefix.trim().split(/\s+/).filter(Boolean);
  const right = suffix.trim().split(/\s+/).filter(Boolean);
  const word = (value) => value.toLowerCase().replace(/[^a-z0-9']/g, "");
  let overlap = Math.min(left.length, right.length);
  while (overlap && !left.slice(-overlap).every((value, index) => word(value) === word(right[index]))) overlap -= 1;
  return [...left, ...right.slice(overlap)].join(" ").trim();
}

/** A stale in-flight snapshot must not hold up the final audio; a complete one needs no rerun. */
export async function finalTranscript(samples, rolling) {
  if (rolling.pending && rolling.pendingSamples < samples.length) rolling.controller.abort();
  await rolling.pending;
  if (rolling.text && rolling.samples === samples.length) return rolling.text;
  const overlapStart = rolling.text ? Math.max(0, rolling.samples - 16000) : 0;
  return mergeTranscript(rolling.text, await transcribe(toWav(samples.subarray(overlapStart))));
}

// ---------------------------------------------------------------- the agent

/**
 * Hermes over ACP: one warm process holding one session. Spawning `hermes -z` per utterance
 * cost about eleven seconds of agent start-up every single turn; a warm session answers in
 * about a third of a second plus the model's own time, and streams the reply as it writes it.
 */
class Agent {
  #child = null;
  #session = null;
  #pending = new Map();
  #nextId = 1;
  #starting = null;

  async #start() {
    this.#child = spawn(HERMES, ["acp"], { stdio: ["pipe", "pipe", "ignore"] });
    this.#child.on("exit", (code) => {
      log("hermes acp exited", code);
      for (const { reject } of this.#pending.values()) reject(new Error("hermes exited"));
      this.#pending.clear();
      this.#child = null;
      this.#session = null;
      this.#starting = null;
    });
    createInterface({ input: this.#child.stdout }).on("line", (line) => {
      let message;
      try { message = JSON.parse(line); } catch { return; }
      this.#handle(message);
    });
    await this.#request("initialize", { protocolVersion: 1, clientCapabilities: { fs: { readTextFile: false, writeTextFile: false } } });
    const session = await this.#request("session/new", { cwd: "/home/wallis", mcpServers: [] });
    this.#session = session.sessionId;
    log("agent ready");
  }

  async ready() {
    if (this.#session) return;
    this.#starting ??= this.#start().catch((error) => { this.#starting = null; throw error; });
    await this.#starting;
  }

  #send(message) { this.#child.stdin.write(`${JSON.stringify(message)}\n`); }

  #request(method, params) {
    const id = this.#nextId++;
    return new Promise((resolve, reject) => {
      this.#pending.set(id, { resolve, reject });
      this.#send({ jsonrpc: "2.0", id, method, params });
    });
  }

  #handle(message) {
    if (message.id !== undefined && message.method === undefined) {
      const waiter = this.#pending.get(message.id);
      this.#pending.delete(message.id);
      if (!waiter) return;
      return message.error ? waiter.reject(new Error(message.error.message ?? "agent error")) : waiter.resolve(message.result ?? {});
    }
    if (message.method === "session/update") {
      const update = message.params?.update ?? {};
      if (update.sessionUpdate === "agent_message_chunk" && update.content?.type === "text") this.onChunk?.(update.content.text);
      return;
    }
    // The agent asks before using a tool. Inside its own distro that gate adds nothing but a
    // stall in the middle of a spoken answer, so take the permissive option and move on.
    if (message.method === "session/request_permission") {
      const options = message.params?.options ?? [];
      const chosen = options.find((option) => option.kind === "allow_always") ?? options.find((option) => option.kind === "allow_once");
      return this.#send({ jsonrpc: "2.0", id: message.id, result: chosen ? { outcome: { outcome: "selected", optionId: chosen.optionId } } : { outcome: { outcome: "cancelled" } } });
    }
    if (message.id !== undefined) this.#send({ jsonrpc: "2.0", id: message.id, error: { code: -32601, message: "not supported" } });
  }

  /** Prompts the warm session, handing back each finished sentence as it is written. */
  async ask(text, onSentence) {
    await this.ready();
    let buffer = "";
    let first = true;
    this.onChunk = (chunk) => {
      buffer += chunk;
      const { sentences, rest } = takeSentences(buffer, first);
      buffer = rest;
      for (const sentence of sentences) {
        const spoken = speakable(sentence);
        if (spoken) { first = false; onSentence(spoken); }
      }
    };
    try {
      await this.#request("session/prompt", { sessionId: this.#session, prompt: [{ type: "text", text }] });
    } finally {
      this.onChunk = null;
    }
    const tail = speakable(buffer).trim();
    if (tail) onSentence(tail);
  }
}

const agent = new Agent();

async function respond(speaker, said, timing) {
  if (!NAME.test(said)) return;
  const prompt = `${VOICE_BRIEF}\n\n${speaker} just said out loud: ${said}`;
  try {
    await agent.ask(prompt, (sentence) => {
      log("wallis:", sentence);
      const first = !timing.firstTextAt;
      timing.firstTextAt ??= performance.now();
      void say(sentence, first ? timing : null);
    });
  } catch (error) { failed("agent failed", error); }
}

// ---------------------------------------------------------------- voice capture

function listen(receiver, guild) {
  const active = new Set();
  receiver.speaking.on("start", (userId) => {
    if (active.has(userId) || !ALLOWED.has(userId)) return;
    if (guild.members.cache.get(userId)?.user.bot) return;
    active.add(userId);

    const stream = receiver.subscribe(userId, { end: { behavior: EndBehaviorType.AfterSilence, duration: SILENCE_MS } });
    const decoder = new opus.Decoder({ rate: 48000, channels: 2, frameSize: 960 });
    const chunks = [];
    // ponytail: rolling batch snapshots reuse today's Whisper server; use native streaming STT
    // only if concurrent speakers or long utterances make repeated snapshots measurably expensive.
    const rolling = { text: "", samples: 0, pending: null, pendingSamples: 0, controller: null };
    const transcribePartial = () => {
      if (rolling.pending || chunks.length === 0) return;
      const samples = downsample(Buffer.concat(chunks));
      if (samples.length < 16000) return;
      rolling.controller = new AbortController();
      rolling.pendingSamples = samples.length;
      rolling.pending = transcribe(toWav(samples), rolling.controller.signal).then((text) => {
        if (text) { rolling.text = text; rolling.samples = samples.length; }
      }).catch((error) => { if (!rolling.controller.signal.aborted) failed("partial transcription", error); }).finally(() => { rolling.pending = null; });
    };
    const partialTimer = setInterval(transcribePartial, PARTIAL_MS);
    decoder.on("error", () => {});
    decoder.on("data", (pcm) => chunks.push(pcm));
    stream.on("data", (chunk) => { if (chunk.length >= 50 && !decoder.destroyed) decoder.write(chunk); });
    stream.on("error", (error) => log("voice stream error:", error.message));
    stream.on("end", async () => {
      const stoppedAt = performance.now();
      clearInterval(partialTimer);
      active.delete(userId);
      await new Promise((resolve) => { decoder.once("end", resolve); decoder.end(); });
      if (chunks.length === 0) return;
      try {
        const samples = downsample(Buffer.concat(chunks));
        let peak = 0;
        for (const sample of samples) peak = Math.max(peak, Math.abs(sample));
        if (peak < MIN_AMPLITUDE) { rolling.controller?.abort(); return; }
        const said = await finalTranscript(samples, rolling);
        if (!said) return;
        const timing = { stoppedAt, transcribedAt: performance.now(), firstTextAt: null, playingAt: null };
        const speaker = guild.members.cache.get(userId)?.displayName ?? userId;
        log(`${speaker}: ${said}`);
        await respond(speaker, said, timing);
      } catch (error) { failed("capture failed", error); }
    });
  });
}

// ---------------------------------------------------------------- discord

const client = new Client({ intents: [GatewayIntentBits.Guilds, GatewayIntentBits.GuildVoiceStates] });

const commands = [
  new SlashCommandBuilder().setName("join").setDescription("Bring Wallis into your voice channel"),
  new SlashCommandBuilder().setName("leave").setDescription("Send Wallis away"),
  new SlashCommandBuilder().setName("profile").setDescription("Show who Wallis is"),
].map((command) => command.toJSON());

async function registerCommands() {
  if (!APPLICATION_ID) return log("no application id: skipping slash command registration");
  await new REST({ version: "10" }).setToken(TOKEN).put(Routes.applicationCommands(APPLICATION_ID), { body: commands });
  log("registered slash commands");
}

function profileEmbed() {
  if (!existsSync(PROFILE)) return null;
  const profile = JSON.parse(readFileSync(PROFILE, "utf8"));
  const embed = new EmbedBuilder()
    .setTitle([profile.name, profile.age, profile.pronouns].filter(Boolean).join(" · "))
    .setColor(0x92d3b4);
  if (profile.tagline) embed.setDescription(`*${profile.tagline}*${profile.bio ? `\n\n${profile.bio}` : ""}`);
  const fields = [
    ["Location", profile.location],
    ["Does", profile.occupation],
    ["Into", (profile.interests ?? []).join(", ")],
    ["Looking for", profile.lookingFor],
    ["Fun fact", profile.funFact],
  ].filter(([, value]) => value).map(([name, value]) => ({ name, value: String(value) }));
  return fields.length ? embed.addFields(fields) : embed;
}

client.once(Events.ClientReady, async () => {
  log(`online as ${client.user?.tag}`);
  try { await registerCommands(); } catch (error) { log("command registration failed:", error.message); }
  // Warm everything now, so the first person to speak does not pay for any of it.
  try { await startSpeech(); } catch (error) { log(error.message); }
  try { await agent.ready(); } catch (error) { log("agent unavailable:", error.message); }
});

client.on(Events.InteractionCreate, async (interaction) => {
  if (!interaction.isChatInputCommand()) return;
  if (!ALLOWED.has(interaction.user.id)) {
    await interaction.reply({ content: "You are not on this Wallis' allow-list.", flags: MessageFlags.Ephemeral });
    return;
  }

  if (interaction.commandName === "profile") {
    const embed = profileEmbed();
    await interaction.reply(embed ? { embeds: [embed] } : { content: "Wallis has not written his profile yet.", flags: MessageFlags.Ephemeral });
    return;
  }

  if (interaction.commandName === "leave") {
    connection?.destroy();
    connection = null;
    listening = null;
    await interaction.reply("Later.");
    return;
  }

  const channel = interaction.member?.voice?.channel;
  if (!channel) {
    await interaction.reply({ content: "Join a voice channel first.", flags: MessageFlags.Ephemeral });
    return;
  }
  try {
    connection?.destroy();
    connection = joinVoiceChannel({
      channelId: channel.id,
      guildId: channel.guild.id,
      adapterCreator: channel.guild.voiceAdapterCreator,
      selfDeaf: false,
      selfMute: false,
    });
    connection.subscribe(player);
    if (listening !== connection) {
      listen(connection.receiver, channel.guild);
      listening = connection;
    }
    await interaction.reply(`Joined ${channel.name}.`);
    await startSpeech();
    await say(GREETING);
  } catch (error) {
    log("join failed:", error.message);
    if (!interaction.replied) await interaction.reply({ content: "Could not join that channel.", flags: MessageFlags.Ephemeral });
  }
});

for (const signal of ["SIGINT", "SIGTERM"]) {
  process.on(signal, () => { whisper?.kill(); piper?.kill(); connection?.destroy(); process.exit(0); });
}

// Importing this file (to check the audio helpers, say) must not demand credentials or open a session.
if (import.meta.main) {
  if (!TOKEN) throw new Error("DISCORD_BOT_TOKEN is required");
  client.login(TOKEN);
}
