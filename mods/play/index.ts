import { Effect } from "effect";
import { randomUUID } from "node:crypto";
import { join } from "node:path";
import { readdirSync, rmSync } from "node:fs";
import { input, emit, reply, runMod, mediaDirectory } from "../_lib/wallis.ts";

runMod(Effect.gen(function* () {
  const request = yield* input;
  const query = String(request.options.query).trim();
  if (!query) return yield* Effect.fail(new Error("Supply a song name or URL."));
  const directory = yield* mediaDirectory;
  const id = randomUUID();
  const output = join(directory, id + ".%(ext)s");
  let queued = false;
  yield* Effect.addFinalizer(() => Effect.sync(() => {
    if (!queued) for (const file of readdirSync(directory)) if (file.startsWith(id + ".")) rmSync(join(directory, file), { force: true });
  }));
  const target = /^https?:\/\//i.test(query) ? query : "ytsearch1:" + query;
  const info = yield* Effect.acquireUseRelease(
    Effect.try(() => Bun.spawn(["/usr/local/bin/yt-dlp", "--ignore-config", "--no-playlist", "--no-cache-dir", "--js-runtimes", "bun:/opt/wallis/tools/bun/bun", "--max-filesize", "50M", "--match-filters", "duration <= 1200", "--socket-timeout", "20", "--retries", "2", "-f", "bestaudio[acodec=opus]/bestaudio", "-o", output, "--print", "after_move:%(.{filepath,title})j", "--", target], { stdin: "pipe", stdout: "pipe", stderr: "inherit" })),
    (child) => Effect.tryPromise(async () => {
      child.stdin.end();
      const text = await new Response(child.stdout).text();
      if (await child.exited !== 0) throw new Error("yt-dlp could not download this song.");
      const info: unknown = JSON.parse(text.trim());
      return info;
    }),
    (child) => Effect.sync(() => { child.kill(); }),
  );
  const song = yield* Effect.try(() => {
    const value = info as { filepath?: unknown; title?: unknown };
    if (typeof value.filepath !== "string" || typeof value.title !== "string") throw new Error("No audio was downloaded (size or duration limit).");
    return { path: value.filepath, title: value.title };
  });
  yield* emit("queue", song);
  queued = true;
  yield* reply("Queued: " + song.title);
}).pipe(Effect.scoped));
