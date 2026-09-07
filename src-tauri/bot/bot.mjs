// Wallis' Discord bot. Runs inside the Wallis WSL2 distro next to Hermes; the desktop app
// installs this file, its dependencies and the local models, then starts it with the
// credentials in the environment. Plain ESM on purpose: no build step, so the desktop app
// can embed this file verbatim and write it into the distro.
//
// Voice loop: Discord opus -> whisper.cpp (local) -> Hermes (`hermes -z`) -> Piper (local) -> Discord.
import { spawn } from "node:child_process";
import { existsSync, readFileSync, unlink } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import {
  AudioPlayerStatus,
  createAudioPlayer,
  createAudioResource,
  EndBehaviorType,
  joinVoiceChannel,
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
const STT_PORT = 8085;
const WHISPER_MODEL = join(HOME, "models", "ggml-base.en.bin");
const VOICE_MODEL = join(HOME, "models", "en_US-ryan-high.onnx");
const GREETING = "Sup boys";
/** Wallis only speaks when spoken to, the way V1 did — his name is the trigger. */
const NAME = /\bwall(?:is|ace|y|ie)\b/i;
const TRANSCRIPT_LIMIT = 12;
const SILENCE_MS = 400;
const MIN_AMPLITUDE = 500;

process.env.FFMPEG_PATH = ffmpegPath;

const log = (...parts) => console.log("[wallis]", ...parts);
const transcript = [];
let connection = null;
let listening = null;
const player = createAudioPlayer();
player.on("error", (error) => log("player error:", error.message));

// ---------------------------------------------------------------- speech to text

let whisper = null;

async function startWhisper() {
  if (whisper) return;
  whisper = spawn(join(HOME, "whisper", "whisper-server"), ["-m", WHISPER_MODEL, "--port", String(STT_PORT), "-t", "4"], { stdio: "ignore" });
  whisper.on("exit", (code) => { log("whisper-server exited", code); whisper = null; });
  for (let attempt = 0; attempt < 60; attempt += 1) {
    try {
      await fetch(`http://127.0.0.1:${STT_PORT}/`);
      log("whisper ready");
      return;
    } catch { await new Promise((resolve) => setTimeout(resolve, 500)); }
  }
  throw new Error("whisper-server did not become ready");
}

async function transcribe(wav) {
  const body = new FormData();
  body.append("file", new Blob([wav], { type: "audio/wav" }), "audio.wav");
  body.append("response_format", "json");
  const response = await fetch(`http://127.0.0.1:${STT_PORT}/inference`, { method: "POST", body });
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

// ---------------------------------------------------------------- text to speech

function speak(text) {
  return new Promise((resolve) => {
    const file = join(tmpdir(), `wallis-${Date.now()}.wav`);
    const piper = spawn(join(HOME, "piper", "bin", "piper"), ["-m", VOICE_MODEL, "-f", file], { stdio: ["pipe", "ignore", "ignore"] });
    piper.stdin.end(text);
    piper.on("error", () => resolve());
    piper.on("close", (code) => {
      if (code !== 0 || !existsSync(file)) return resolve();
      player.play(createAudioResource(file));
      player.once(AudioPlayerStatus.Idle, () => unlink(file, () => {}));
      resolve();
    });
  });
}

// ---------------------------------------------------------------- the agent

function askHermes(prompt) {
  return new Promise((resolve) => {
    const hermes = spawn(HERMES, ["-z", prompt], { stdio: ["ignore", "pipe", "ignore"] });
    let answer = "";
    hermes.stdout.on("data", (chunk) => { answer += chunk; });
    const timer = setTimeout(() => hermes.kill(), 120_000);
    hermes.on("error", () => { clearTimeout(timer); resolve(""); });
    hermes.on("close", () => { clearTimeout(timer); resolve(answer.trim()); });
  });
}

/** Strip anything that would be read aloud as punctuation soup. */
export function speakable(text) {
  return text.replace(/```[\s\S]*?```/g, "").replace(/[*_`#>]/g, "").split("\n").map((line) => line.trim()).filter(Boolean).join(" ").slice(0, 600);
}

async function respond(speaker, said) {
  transcript.push(`${speaker}: ${said}`);
  while (transcript.length > TRANSCRIPT_LIMIT) transcript.shift();
  if (!NAME.test(said)) return;
  const prompt = [
    "You are in a Discord voice call with friends and someone just addressed you out loud.",
    "Recent transcript:",
    transcript.join("\n"),
    "",
    "Answer out loud in one or two short spoken sentences. Plain speech only: no markdown, no emoji, no stage directions, no lists.",
  ].join("\n");
  const answer = speakable(await askHermes(prompt));
  if (!answer) return;
  log("wallis:", answer);
  transcript.push(`Wallis: ${answer}`);
  await speak(answer);
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
    decoder.on("error", () => {});
    decoder.on("data", (pcm) => chunks.push(pcm));
    stream.on("data", (chunk) => { if (chunk.length >= 50 && !decoder.destroyed) decoder.write(chunk); });
    stream.on("error", (error) => log("voice stream error:", error.message));
    stream.on("end", async () => {
      active.delete(userId);
      decoder.end();
      if (chunks.length === 0) return;
      try {
        const samples = downsample(Buffer.concat(chunks));
        let peak = 0;
        for (const sample of samples) peak = Math.max(peak, Math.abs(sample));
        if (peak < MIN_AMPLITUDE) return;
        const said = await transcribe(toWav(samples));
        if (!said) return;
        const speaker = guild.members.cache.get(userId)?.displayName ?? userId;
        log(`${speaker}: ${said}`);
        await respond(speaker, said);
      } catch (error) { log("capture failed:", error.message); }
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
  try { await startWhisper(); } catch (error) { log(error.message); }
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
    await startWhisper();
    await speak(GREETING);
  } catch (error) {
    log("join failed:", error.message);
    if (!interaction.replied) await interaction.reply({ content: "Could not join that channel.", flags: MessageFlags.Ephemeral });
  }
});

for (const signal of ["SIGINT", "SIGTERM"]) {
  process.on(signal, () => { whisper?.kill(); connection?.destroy(); process.exit(0); });
}

// Importing this file (to check the audio helpers, say) must not demand credentials or open a session.
if (import.meta.main) {
  if (!TOKEN) throw new Error("DISCORD_BOT_TOKEN is required");
  client.login(TOKEN);
}
