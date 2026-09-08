import { describe, expect, test } from "bun:test";
import { once } from "node:events";
import { PassThrough } from "node:stream";
import { opus } from "prism-media";
import { encodeSpeech, finalTranscript, mergeTranscript, takeSentences } from "./bot.mjs";

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
