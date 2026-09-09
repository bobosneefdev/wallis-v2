import { expect, test } from "bun:test";
import { EventEmitter, once } from "node:events";
import { PassThrough } from "node:stream";
import { opus } from "prism-media";
import { Agent, createVoiceCoordinator, deliverAction, dispatchBackground, encodeSpeech, enqueueBackground, finalTranscript, isAllowed, joinCall, mergeTranscript, responsePrompt, shouldConsider, takeActions } from "./bot.mjs";

test("allow-list wildcard permits anyone", () => {
  expect(isAllowed("123", new Set(["*"]))).toBe(true);
  expect(isAllowed("123", new Set(["456"]))).toBe(false);
});

test("join acknowledges Discord before connecting and reports only a ready call", async () => {
  const events = [];
  const interaction = {
    deferReply: async () => events.push("deferred"),
    editReply: async (message) => events.push(message),
  };
  const call = { subscribe: () => events.push("subscribed"), destroy: () => events.push("destroyed") };
  const channel = { id: "voice", name: "General", guild: { id: "guild", voiceAdapterCreator: {} } };
  const connect = () => { events.push("connecting"); return call; };
  const ready = async () => events.push("ready");

  expect(await joinCall(interaction, channel, connect, ready, {})).toBe(call);
  expect(events).toEqual(["deferred", "connecting", "subscribed", "ready", "Joined General."]);

  events.length = 0;
  await expect(joinCall(interaction, channel, connect, async () => { throw new Error("timed out"); }, {})).rejects.toThrow("timed out");
  expect(events).toEqual(["deferred", "connecting", "subscribed", "destroyed"]);
});

test("merges the overlapping final transcription", () => {
    expect(mergeTranscript("Wallis what did we decide about the trip", "about the trip next weekend?")).toBe(
      "Wallis what did we decide about the trip next weekend?",
    );
});

test("routing streams complete speech objects and keeps code inside file actions", () => {
  const actions = [{ type: "say", text: "I'll put that together." }, { type: "file", filename: "snake.html", content: '<html>\n<script>const snake = [];</script>\n</html>' }];
  let buffer = "";
  const received = [];
  for (const character of actions.map((action) => JSON.stringify(action)).join("\n")) {
    const parsed = takeActions(buffer + character);
    buffer = parsed.rest;
    received.push(...parsed.actions);
  }
  expect(received).toEqual(actions);
  expect(buffer).toBe("");
  expect(() => takeActions('raw code is not speech', true)).toThrow();
  expect(() => takeActions(JSON.stringify({ type: "say", text: '<script>alert(1)</script>' }))).toThrow();
  expect(() => takeActions(JSON.stringify({ type: "file", filename: "../secrets.txt", content: "x" }))).toThrow();
  expect(() => takeActions("x".repeat(512001))).toThrow();
});

test("natural participation permits relevant opportunities and follow-ups without a name trigger", () => {
  const turn = { mode: "voice", userId: "Jake", direct: false, participation: "natural" };
  const state = { pending: 0, nextConsiderAt: 20000, lastReplyAt: 0, lastReplyUser: null };
  expect(shouldConsider(turn, state, 1000)).toBe(false);
  expect(shouldConsider(turn, state, 20000)).toBe(true);
  expect(shouldConsider(turn, { ...state, pending: 1 }, 20000)).toBe(false);
  expect(shouldConsider({ ...turn, speaking: true }, state, 20000)).toBe(false);
  expect(shouldConsider({ ...turn, direct: true, participation: "mentions" }, state, 1000)).toBe(true);
  expect(shouldConsider({ ...turn, participation: "mentions" }, state, 20000)).toBe(false);
  expect(shouldConsider(turn, { ...state, lastReplyUser: "Jake", lastReplyAt: 500 }, 1000)).toBe(true);
  expect(shouldConsider({ ...turn, mode: "text" }, { ...state, lastReplyUser: "Jake", lastReplyAt: 500 }, 1000)).toBe(false);
});

test("voice coordination joins thought pauses, flushes fluid talk and steers its one active turn", async () => {
  let now = 0;
  const jobs = new Set();
  const timers = {
    set: (run, delay) => { const job = { at: now + delay, run }; jobs.add(job); return job; },
    clear: (job) => jobs.delete(job),
    advance: async (delay) => {
      now += delay;
      for (const job of [...jobs].filter(({ at }) => at <= now).sort((a, b) => a.at - b.at)) {
        jobs.delete(job);
        job.run();
        await Promise.resolve();
      }
    },
  };
  const turns = [], steers = [];
  let release;
  const blocked = new Promise((resolve) => { release = resolve; });
  const coordinator = createVoiceCoordinator(async (segments) => { turns.push(segments.map(({ text }) => text)); await blocked; },
    { quietMs: 350, maxMs: 900, steer: async ({ text }) => { steers.push(text); return true; }, setTimer: timers.set, clearTimer: timers.clear });

  coordinator.speaking();
  coordinator.push({ text: "Wallis, I was thinking" });
  coordinator.roomQuiet();
  await timers.advance(200);
  coordinator.speaking();
  coordinator.push({ text: "we should leave tomorrow" });
  coordinator.roomQuiet();
  await timers.advance(349);
  expect(turns).toEqual([]);
  await timers.advance(1);
  expect(turns).toEqual([["Wallis, I was thinking", "we should leave tomorrow"]]);

  coordinator.speaking();
  coordinator.push({ text: "A fluid conversation" });
  expect(steers).toEqual(["A fluid conversation"]);
  release();
  await new Promise((resolve) => setTimeout(resolve, 0));

  coordinator.speaking();
  coordinator.push({ text: "The next fluid turn" });
  await timers.advance(900);
  expect(turns.at(-1)).toEqual(["The next fluid turn"]);

  coordinator.hold();
  coordinator.speaking();
  coordinator.push({ text: "Captured while Wallis owns the floor" });
  coordinator.roomQuiet();
  await timers.advance(900);
  expect(turns).toHaveLength(2);
  coordinator.resume();
  await timers.advance(350);
  expect(turns.at(-1)).toEqual(["Captured while Wallis owns the floor"]);
});

test("voice Snake request posts the complete HTML before speaking a confirmation; failures never confirm", async () => {
  const events = [];
  const channel = { guildId: "guild", name: "general", isThread: () => false, permissionsFor: () => ({ has: () => true }), send: async (payload) => events.push(payload) };
  const turn = { mode: "voice", guild: { id: "guild", members: { me: {} } }, defaultChannel: "general-id", channels: new Map([["general-id", channel]]) };
  const action = { type: "file", filename: "snake.html", content: "<!doctype html><script>const snake = [];</script>" };
  const speak = async (text) => { events.push(text); return true; };
  await deliverAction(action, turn, speak);
  expect(events[0].files[0].attachment.toString()).toBe(action.content);
  expect(events[0].files[0].name).toBe("snake.html");
  expect(events[0].allowedMentions).toEqual({ parse: [], repliedUser: false });
  expect(events[1]).toBe("I put the file in general. Go check it out.");

  events.length = 0;
  channel.send = async () => { throw new Error("upload failed"); };
  await expect(deliverAction(action, turn, speak)).rejects.toThrow("upload failed");
  expect(events).toEqual([]);
  await expect(deliverAction({ ...action, channelId: "unapproved" }, turn, speak)).rejects.toThrow("allowed write");
  channel.guildId = "another-server";
  await expect(deliverAction(action, turn, speak)).rejects.toThrow("allowed write");
  channel.guildId = "guild";
  channel.permissionsFor = () => ({ has: () => false });
  await expect(deliverAction(action, turn, speak)).rejects.toThrow("Missing Discord permissions");
});

test("text replies stay in text and long replies become attachments without mass mentions", async () => {
  const sent = [];
  const channel = { guildId: "guild", name: "general", isThread: () => false, permissionsFor: () => ({ has: () => true }), send: async (payload) => sent.push(payload) };
  const turn = { mode: "text", guild: { id: "guild", members: { me: {} } }, defaultChannel: "general-id", channelId: "general-id", messageId: "message", channels: new Map([["general-id", channel]]) };
  await deliverAction({ type: "message", text: "@everyone hey" }, turn);
  expect(sent[0].content).toBe("@everyone hey");
  expect(sent[0].allowedMentions.parse).toEqual([]);
  expect(sent[0].reply.messageReference).toBe("message");
  await deliverAction({ type: "message", text: "a".repeat(2001) }, turn);
  expect(sent[1].files[0].name).toBe("reply.md");
  await expect(deliverAction({ type: "say", text: "Hello" }, turn)).rejects.toThrow("text mode");
  expect(await deliverAction({ type: "skip" }, turn)).toBe(false);
  expect(responsePrompt(turn, [])).toContain('"mode":"text"');
  expect(responsePrompt(turn, [])).not.toContain("interpret names and homophones by sound");
  expect(responsePrompt({ ...turn, mode: "voice" }, [])).toContain("interpret names and homophones by sound");
});

test("realtime prompts serialize while steering stays on the active voice callback", async () => {
  let sessions = 0, activePrompts = 0, maxActivePrompts = 0;
  const models = [];
  const child = new EventEmitter();
  const steers = [];
  child.stdout = new PassThrough();
  child.stdin = { write: (line) => {
    const request = JSON.parse(line);
    if (request.method === "session/set_model") models.push(request.params);
    if (request.method === "session/prompt" && !request.params.prompt[0].text.startsWith("/steer ")) {
      activePrompts += 1;
      maxActivePrompts = Math.max(maxActivePrompts, activePrompts);
    }
    setTimeout(() => {
      if (request.method === "session/prompt") {
        const text = request.params.prompt[0].text;
        if (text.startsWith("/steer ")) {
          steers.push(text);
          child.stdout.write(JSON.stringify({ method: "session/update", params: { sessionId: request.params.sessionId, update: { sessionUpdate: "agent_message_chunk", content: { type: "text", text: `⏩ Steer queued for the active turn: ${text.slice(7)}` } } } }) + "\n");
        } else child.stdout.write(JSON.stringify({ method: "session/update", params: { sessionId: request.params.sessionId, update: { sessionUpdate: "agent_message_chunk", content: { type: "text", text } } } }) + "\n");
      }
      if (request.method === "session/prompt" && !request.params.prompt[0].text.startsWith("/steer ")) activePrompts -= 1;
      child.stdout.write(JSON.stringify({ id: request.id, result: request.method === "session/new" ? { sessionId: "session-" + ++sessions } : {} }) + "\n");
    }, request.method === "session/prompt" && !request.params.prompt[0].text.startsWith("/steer ") ? 20 : 0);
  } };
  const agent = new Agent((_command, args, { env }) => {
    expect(args[0]).toEndWith("hermes_acp.py");
    expect(env.WALLIS_REASONING_EFFORT).toBe("high");
    expect(env.DISCORD_BOT_TOKEN).toBeUndefined();
    expect(env.WALLIS_BACKGROUND_API_KEY).toBeUndefined();
    return child;
  }, "openrouter:background-model", {}, "high"), voice = [], text = [];
  await agent.ready();
  const replies = Promise.all([agent.ask("voice reply", (chunk) => voice.push(chunk), "voice"), agent.ask("text reply", (chunk) => text.push(chunk), "text")]);
  await new Promise((resolve) => setTimeout(resolve, 0));
  expect(await agent.steer("new voice window", "voice")).toBe(true);
  await replies;
  expect(voice).toEqual(["voice reply"]);
  expect(text).toEqual(["text reply"]);
  expect(maxActivePrompts).toBe(1);
  expect(models).toEqual([{ sessionId: "session-1", modelId: "openrouter:background-model" }, { sessionId: "session-2", modelId: "openrouter:background-model" }]);
  expect(steers).toEqual(["/steer new voice window"]);
  child.stdout.end();
});

test("handoff returns after acknowledgement while the background model is still working", async () => {
  const sent = [];
  let release;
  const blocked = new Promise((resolve) => { release = resolve; });
  const channel = { guildId: "guild", name: "general", isThread: () => false, permissionsFor: () => ({ has: () => true }), send: async (payload) => sent.push(payload) };
  const turn = { mode: "text", guild: { id: "guild", members: { me: {} } }, defaultChannel: "general", channelId: "general", channels: new Map([["general", channel]]), said: "Write a Python script", speaker: "Jake" };
  const worker = { model: "background-model", ask: async (prompt, onChunk) => {
    expect(prompt).toContain("Write a Python script");
    await blocked;
    onChunk(JSON.stringify({ type: "file", filename: "script.py", content: "print('hello')" }));
  } };
  const { completion } = await dispatchBackground({ type: "delegate", task: "Write a Python script" }, turn, [], null, worker);
  expect(sent.length).toBe(1);
  expect(sent[0].content).toContain("background");
  // The foreground caller continues before the slow job is released.
  await deliverAction({ type: "message", text: "Still here — we can keep chatting." }, turn);
  expect(sent.length).toBe(2);
  release();
  await completion;
  expect(sent[2].files[0].attachment.toString()).toBe("print('hello')");
  await expect(dispatchBackground({ type: "delegate", task: "x", channelId: "not-allowed" }, turn, [], null, worker)).rejects.toThrow("allowed text channel");
  await expect(dispatchBackground({ type: "delegate", task: "x" }, turn, [], null, { model: "" })).rejects.toThrow("Configure a background model");
  expect(() => takeActions(JSON.stringify({ type: "delegate", task: "" }))).toThrow();
});

test("background queue is bounded and a failed job does not block subsequent jobs", async () => {
  let release;
  const blocked = new Promise((resolve) => { release = resolve; });
  const first = enqueueBackground(async () => { await blocked; throw new Error("job failed"); });
  const failed = first.catch((error) => error.message);
  const rest = [1, 2, 3].map((value) => enqueueBackground(async () => value));
  expect(() => enqueueBackground(async () => 4)).toThrow("queue is full");
  release();
  expect(await failed).toBe("job failed");
  expect(await Promise.all(rest)).toEqual([1, 2, 3]);
});

test("Opus reaches Discord before Piper finishes its PCM response", async () => {
  const audio = new PassThrough();
  const encoded = encodeSpeech(audio);
  const demuxer = encoded.pipe(new opus.OggDemuxer());
  const packet = once(demuxer, "data", { signal: AbortSignal.timeout(1500) });
  // Only 200 ms of PCM, with the response left open. Headers alone are not playable audio.
  audio.write(Buffer.alloc(8820));
  try {
    const [data] = await packet;
    expect(data.length).toBeGreaterThan(0);
    expect(audio.writableEnded).toBe(false);
  } finally {
    audio.end();
    demuxer.resume();
    await once(encoded, "close");
  }
});

test("final transcription reuses complete snapshots and cancels stale work without dropping the tail", async () => {
  const fetch = globalThis.fetch;
  const requests = [];
  globalThis.fetch = async (_url, { body }) => {
    requests.push(body);
    return Response.json({ text: "the trip next weekend?" });
  };
  try {
    const samples = new Int16Array(48000);
    const rolling = { text: "Wallis what about the trip", samples: samples.length, pending: null };
    expect(await finalTranscript(samples, rolling)).toBe(rolling.text);
    expect(requests.length).toBe(0);

    rolling.text = "";
    rolling.samples = 0;
    rolling.pendingSamples = samples.length;
    rolling.controller = new AbortController();
    rolling.pending = Promise.resolve().then(() => {
      rolling.text = "Wallis what about the trip";
      rolling.samples = samples.length;
    });
    expect(await finalTranscript(samples, rolling)).toBe(rolling.text);
    expect(rolling.controller.signal.aborted).toBe(false);
    expect(requests.length).toBe(0);

    rolling.samples = 32000;
    rolling.pendingSamples = 40000;
    rolling.pending = new Promise((resolve) => rolling.controller.signal.addEventListener("abort", resolve, { once: true }));
    expect(await finalTranscript(samples, rolling)).toBe("Wallis what about the trip next weekend?");
    expect(rolling.controller.signal.aborted).toBe(true);
    expect(requests.length).toBe(1);
    expect(requests[0].get("no_timestamps")).toBe("true");
    expect(requests[0].get("token_timestamps")).toBe("false");
    expect(requests[0].get("file").size).toBe(44 + 32000 * 2);

    expect(await finalTranscript(samples, { text: "", samples: 0, pending: null })).toBe("the trip next weekend?");
    expect(requests[1].get("file").size).toBe(44 + samples.length * 2);
  } finally {
    globalThis.fetch = fetch;
  }
});
