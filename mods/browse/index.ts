import { Effect } from "effect";
import { input, emit, reply, runMod } from "../_lib/wallis.ts";
runMod(Effect.gen(function* () {
  const request = yield* input;
  const result = yield* emit("delegate", { task: "Use the browser skill to research: " + request.options.question, context: "Requested by Discord user " + request.user, channelId: request.channel });
  yield* reply(result);
}));
