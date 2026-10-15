import { Cause, Config, Effect, Schema } from "effect";
import { BunRuntime } from "@effect/platform-bun";
import { createInterface } from "node:readline";

const Input = Schema.Struct({
  options: Schema.Record(Schema.String, Schema.Union([Schema.String, Schema.Number, Schema.Boolean])),
  user: Schema.String, guild: Schema.String, channel: Schema.String,
});
export const decodeInput = Schema.decodeUnknownSync(Input);
let lines: AsyncIterableIterator<string> | undefined;
let reader: ReturnType<typeof createInterface> | undefined;
const line = Effect.tryPromise(async () => {
  lines ??= (reader = createInterface({ input: process.stdin }))[Symbol.asyncIterator]();
  const next = await lines.next();
  if (next.done) throw new Error("Host closed stdin.");
  return next.value;
});
export const input = line.pipe(Effect.flatMap((s) => Effect.try(() => decodeInput(JSON.parse(s)))));
export const emit = Effect.fn("wallis.emit")(function* (tool: string, args: Record<string, unknown> = {}) {
  yield* Effect.sync(() => process.stdout.write(JSON.stringify({ tool, args }) + "\n"));
  return yield* line.pipe(Effect.flatMap((s) => Effect.try(() => {
    const { result } = Schema.decodeUnknownSync(Schema.Struct({ result: Schema.String }))(JSON.parse(s));
    if (result.startsWith("Failed:")) throw new Error(result);
    return result;
  })));
});
export const reply = (text: string) => emit("reply", { text });
export const mediaDirectory = Config.String("WALLIS_MEDIA");
export const runMod = <A, E>(effect: Effect.Effect<A, E>) => BunRuntime.runMain(effect.pipe(Effect.ensuring(Effect.sync(() => reader?.close())), Effect.tapCause((cause) => Effect.sync(() => console.error(Cause.pretty(cause))))), { disableErrorReporting: true });
