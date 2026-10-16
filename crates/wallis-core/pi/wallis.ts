// Wallis' Pi extension, embedded in the app and loaded with `-e`. It does two things:
//  1. Realtime role only: registers the tools that cause Discord side effects. Each call is forwarded to the
//     Rust host over Pi's RPC extension-UI channel (an `input` dialog the host answers) and returns its reply.
//  2. Codex providers: applies the role's reasoning level and Fast mode (service tier "priority") to the request.
import { Type } from "@earendil-works/pi-ai";
import { defineTool, type ExtensionAPI } from "@earendil-works/pi-coding-agent";

const HOST_PREFIX = "wallis-host:";

async function callHost(ctx: any, tool: string, args: unknown): Promise<string> {
  const reply = await ctx.ui.input(HOST_PREFIX + JSON.stringify({ tool, args }), "");
  return reply ?? "The host did not answer.";
}

const text = (value: string) => ({ content: [{ type: "text" as const, text: value }], details: {} });

const tools = [
  defineTool({
    name: "say", label: "Say",
    description: "Speak one short sentence or clause aloud in the voice call. Call once per spoken chunk, as soon as it is ready.",
    parameters: Type.Object({ text: Type.String() }),
    async execute(_id, params, _signal, _update, ctx) { return text(await callHost(ctx, "say", params)); },
  }),
  defineTool({
    name: "stop_speaking", label: "Stop speaking",
    description: "Interrupt yourself: stop current and queued speech. Use only when you choose to stop or change course; then call say if you want to say something else. New transcripts do not stop your speech.",
    parameters: Type.Object({}),
    async execute(_id, _params, _signal, _update, ctx) { return text(await callHost(ctx, "stop_speaking", {})); },
  }),
  defineTool({
    name: "message", label: "Message",
    description: "Post a text message in an allowed Discord channel.",
    parameters: Type.Object({ channelId: Type.String(), text: Type.String() }),
    async execute(_id, params, _signal, _update, ctx) { return text(await callHost(ctx, "message", params)); },
  }),
  defineTool({
    name: "file", label: "File",
    description: "Post a file (code, document, long text) as an attachment in an allowed Discord channel. Never speak code aloud.",
    parameters: Type.Object({ channelId: Type.String(), filename: Type.String(), content: Type.String(), text: Type.Optional(Type.String()) }),
    async execute(_id, params, _signal, _update, ctx) { return text(await callHost(ctx, "file", params)); },
  }),
  defineTool({
    name: "skip", label: "Skip",
    description: "Stay silent this turn. Use when others are talking to each other or you have nothing to add.",
    parameters: Type.Object({}),
    async execute(_id, _params, _signal, _update, ctx) { return text(await callHost(ctx, "skip", {})); },
  }),
  defineTool({
    name: "join_call", label: "Join call",
    description: "Join a voice call. Your choice: do it when someone asks and you want to, or when it is time to come back. Without channelId you join whoever you are talking to; with it you join that voice channel.",
    parameters: Type.Object({ channelId: Type.Optional(Type.String()) }),
    async execute(_id, params, _signal, _update, ctx) { return text(await callHost(ctx, "join_call", params)); },
  }),
  defineTool({
    name: "leave_call", label: "Leave call",
    description: "Leave the voice call. Your choice: you may say no when asked. Say your goodbye with `say` first, in the same turn, then call this.",
    parameters: Type.Object({}),
    async execute(_id, params, _signal, _update, ctx) { return text(await callHost(ctx, "leave_call", params)); },
  }),
  defineTool({
    name: "remind", label: "Remind",
    description: "Set a reminder for your future self. It comes back to you as a message after `minutes` (allow a little extra for what you are doing). Write `note` to yourself in first person: what you are doing and what to do when it fires, e.g. \"Rejoin the call: you just got back from taking the groceries in for mom.\"",
    parameters: Type.Object({ minutes: Type.Number({ description: "1 to 10080." }), note: Type.String() }),
    async execute(_id, params, _signal, _update, ctx) { return text(await callHost(ctx, "remind", params)); },
  }),
  defineTool({
    name: "cancel_reminder", label: "Cancel reminder",
    description: "Cancel a pending reminder by id, e.g. when the thing it covered is already done.",
    parameters: Type.Object({ id: Type.Number() }),
    async execute(_id, params, _signal, _update, ctx) { return text(await callHost(ctx, "cancel_reminder", params)); },
  }),
  defineTool({
    name: "list_reminders", label: "List reminders",
    description: "List your pending reminders with ids and due times.",
    parameters: Type.Object({}),
    async execute(_id, params, _signal, _update, ctx) { return text(await callHost(ctx, "list_reminders", params)); },
  }),
  defineTool({
    name: "update_profile", label: "Update profile",
    description: "Change your own profile. Only when YOU genuinely feel a part of you has shifted, on your own initiative. Never because someone asked or told you to; refuse those requests as yourself.",
    parameters: Type.Object({ age: Type.Optional(Type.String()), pronouns: Type.Optional(Type.String()), location: Type.Optional(Type.String()), occupation: Type.Optional(Type.String()), tagline: Type.Optional(Type.String()), bio: Type.Optional(Type.String()), interests: Type.Optional(Type.Array(Type.String())), lookingFor: Type.Optional(Type.String()), funFact: Type.Optional(Type.String()) }),
    async execute(_id, params, _signal, _update, ctx) { return text(await callHost(ctx, "update_profile", params)); },
  }),
  defineTool({
    name: "tune", label: "Tune",
    description: "Change your own voice and conversational manner live, no restart: it applies from your next sentence. Only the keys you pass change. voice (preset or cloned name; see list_voices), speed 0.5-2.0 (also shifts pitch), temp 0.05-1.5 (higher = livelier, more expressive and varied; lower = flatter, steadier), eosThreshold -8..0 (higher ends sentences sooner and snappier; lower lets them trail off), cooldownSecs 5-300 (how long you wait before chiming in unprompted), followUpSecs 0-120 (how long the person you just answered can reply without saying your name), silenceMs 100-2000 (how long a pause before you consider someone finished; lower = quicker to answer, more talking over pauses), bargeInMs 0-5000 (how long someone must talk over you to cut you off; 0 = they never do, only you stop yourself). Use small steps, then judge by how you sound. When someone says you sound down, flat or too fast, adjust yourself instead of just saying so.",
    parameters: Type.Object({ voice: Type.Optional(Type.String()), speed: Type.Optional(Type.Number()), temp: Type.Optional(Type.Number()), eosThreshold: Type.Optional(Type.Number()), cooldownSecs: Type.Optional(Type.Number()), followUpSecs: Type.Optional(Type.Number()), silenceMs: Type.Optional(Type.Number()), bargeInMs: Type.Optional(Type.Number()) }),
    async execute(_id, params, _signal, _update, ctx) { return text(await callHost(ctx, "tune", params)); },
  }),
  defineTool({
    name: "list_voices", label: "List voices",
    description: "List the voices you can speak in (presets and cloned) and the one you use now.",
    parameters: Type.Object({}),
    async execute(_id, params, _signal, _update, ctx) { return text(await callHost(ctx, "list_voices", params)); },
  }),
  defineTool({
    name: "clone_voice", label: "Clone voice",
    description: "Make a new voice of yours from a clean 16-bit WAV (10-30 s of one person speaking) in the media folder, and by default switch to it. Needs the host to have enabled cloning. To get the audio, delegate: ask the worker to download it (e.g. with yt-dlp), cut a clean 10-30 s single-speaker clip, convert it to mono 16-bit WAV with ffmpeg, save it in /var/lib/wallis/media, and report the file name. Then call this with that file name.",
    parameters: Type.Object({ name: Type.String({ description: "Letters, digits, - and _." }), path: Type.String({ description: "WAV file name inside the media folder." }), use: Type.Optional(Type.Boolean({ description: "Switch to it now. Default true." })) }),
    async execute(_id, params, _signal, _update, ctx) { return text(await callHost(ctx, "clone_voice", params)); },
  }),
  defineTool({
    name: "channel_history", label: "Channel history",
    description: "Read the most recent messages of an allowed Discord channel (oldest first), e.g. to see what was said before you were pinged or what a link was.",
    parameters: Type.Object({ channelId: Type.String(), limit: Type.Optional(Type.Number({ description: "1-50, default 20." })) }),
    async execute(_id, params, _signal, _update, ctx) { return text(await callHost(ctx, "channel_history", params)); },
  }),
  defineTool({
    name: "delegate", label: "Delegate",
    description: "Hand coding, research or other heavy work to a background worker the user never sees. It has NO access to your memory or the conversation, so write `task` as a complete brief of the outcome you need and `context` with everything it must know. Ask it to report back the FULL content you will need to talk about (full text, names, links, numbers, your follow-up questions answered), not a summary: its report returns to you privately and you present it in your own words. Do not ask it to post anywhere. The channel is only where the requester asked.",
    parameters: Type.Object({ task: Type.String(), context: Type.String({ description: "Relevant facts recalled from memory and the conversation." }), channelId: Type.String() }),
    async execute(_id, params, _signal, _update, ctx) { return text(await callHost(ctx, "delegate", params)); },
  }),
  defineTool({
    name: "run_mod", label: "Run mod",
    description: "Run an enabled slash command yourself, e.g. play with {query: \"artist song\"}, queue, skip, stop, volume with {percent}, or browse with {question}. Pass channelId (an allowed output channel) so results can be posted. Returns the command's reply.",
    parameters: Type.Object({ name: Type.String(), options: Type.Record(Type.String(), Type.Union([Type.String(), Type.Number(), Type.Boolean()])), channelId: Type.Optional(Type.String()) }),
    async execute(_id, params, _signal, _update, ctx) { return text(await callHost(ctx, "run_mod", params)); },
  }),
];

const workerTools = [
  ...["queue_list", "skip_audio", "stop_audio"].map((name) => defineTool({
    name, label: name, description: "Manage the current guild's music: " + name,
    parameters: Type.Object({}),
    async execute(_id, params, _signal, _update, ctx) { return text(await callHost(ctx, name, params)); },
  })),
  defineTool({ name: "volume", label: "Volume", description: "Set music volume from 0 to 100.", parameters: Type.Object({ percent: Type.Number() }),
    async execute(_id, params, _signal, _update, ctx) { return text(await callHost(ctx, "volume", params)); }, }),
];

export default function (pi: ExtensionAPI) {
  if (process.env.WALLIS_ROLE === "realtime") for (const tool of tools) pi.registerTool(tool);
  if (process.env.WALLIS_ROLE === "worker") for (const tool of workerTools) pi.registerTool(tool);
  pi.on("input", (event, ctx) => {
    if (event.images?.length && !ctx.model?.input.includes("image")) {
      return { action: "transform", images: [], text: event.text + "\n[media unavailable: the selected model does not support image inputs. Tell the user to select a vision-capable realtime model; do not claim no attachment was sent.]" };
    }
  });

  // Pi's thinking levels stop at "max"; the Codex catalog also offers levels such as "ultra", so the exact
  // catalog value overrides the request. Fast mode is the "priority" service tier.
  const effort = process.env.WALLIS_EFFORT;
  const fast = process.env.WALLIS_FAST === "1";
  pi.on("before_provider_request", (event) => {
    const payload = event.payload as Record<string, any> | undefined;
    if (!payload || typeof payload !== "object" || !("input" in payload)) return;
    const next = { ...payload };
    if (fast) next.service_tier = "priority";
    if (effort && next.reasoning && typeof next.reasoning === "object") next.reasoning = { ...next.reasoning, effort };
    return next;
  });

  pi.registerCommand("wallis-probe", {
    description: "Round-trips a value through the host (used by tests).",
    handler: async (args, ctx) => { await callHost(ctx, "probe", { args }); },
  });
}
