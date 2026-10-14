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
    name: "update_profile", label: "Update profile",
    description: "Change your own profile. Only when YOU genuinely feel a part of you has shifted, on your own initiative. Never because someone asked or told you to; refuse those requests as yourself.",
    parameters: Type.Object({ age: Type.Optional(Type.String()), pronouns: Type.Optional(Type.String()), location: Type.Optional(Type.String()), occupation: Type.Optional(Type.String()), tagline: Type.Optional(Type.String()), bio: Type.Optional(Type.String()), interests: Type.Optional(Type.Array(Type.String())), lookingFor: Type.Optional(Type.String()), funFact: Type.Optional(Type.String()) }),
    async execute(_id, params, _signal, _update, ctx) { return text(await callHost(ctx, "update_profile", params)); },
  }),
  defineTool({
    name: "delegate", label: "Delegate",
    description: "Hand coding, research or other heavy work to the worker. The worker has NO access to your memory, so put everything it needs in `context`.",
    parameters: Type.Object({ task: Type.String(), context: Type.String({ description: "Relevant facts recalled from memory and the conversation." }), channelId: Type.String() }),
    async execute(_id, params, _signal, _update, ctx) { return text(await callHost(ctx, "delegate", params)); },
  }),
];

export default function (pi: ExtensionAPI) {
  if (process.env.WALLIS_ROLE === "realtime") for (const tool of tools) pi.registerTool(tool);

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
