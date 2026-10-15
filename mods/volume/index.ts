import { Effect } from "effect";
import { input, emit, reply, runMod } from "../_lib/wallis.ts";

runMod(Effect.gen(function* () {
  const request = yield* input;
  yield* reply(yield* emit("volume", request.options));
}));
