import { describe, expect, test } from "bun:test";
import { mergeTranscript, takeSentences } from "./bot.mjs";

describe("streaming voice boundaries", () => {
  test("releases one substantial opening clause before sentence completion", () => {
    expect(takeSentences("I remember exactly what you mean, but let me check", true)).toEqual({
      sentences: ["I remember exactly what you mean,"],
      rest: "but let me check",
    });
    expect(takeSentences("Sure, let me check", true).sentences).toEqual([]);
  });

  test("merges the overlapping final transcription", () => {
    expect(mergeTranscript("Wallis what did we decide about the trip", "about the trip next weekend?")).toBe(
      "Wallis what did we decide about the trip next weekend?",
    );
  });
});
