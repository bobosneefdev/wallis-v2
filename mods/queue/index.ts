import { Effect } from "effect";
import { input, emit, reply, runMod } from "../_lib/wallis.ts";

runMod(Effect.gen(function* () {
  yield* input;
  yield* reply(yield* emit("queue_list", {}));
}));
