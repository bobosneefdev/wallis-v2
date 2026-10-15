import { strict as assert } from "node:assert";
import { test } from "bun:test";
import { decodeInput } from "./wallis.ts";
test("mod input validates scalar options and Discord context", () => {
  const input = { options: { query: "a", percent: 40, flag: true }, user: "1", guild: "2", channel: "3" };
  assert.deepEqual(decodeInput(input), input);
  assert.throws(() => decodeInput({ ...input, options: { query: [] } }));
  assert.throws(() => decodeInput({ options: {} }));
});

import { Effect } from "effect";
import { createInterface } from "node:readline";
import { Readable } from "node:stream";
import { fileURLToPath } from "node:url";
test("a stock command exchanges actions and results without corrupting JSON lines", () => Effect.runPromise(Effect.tryPromise(async () => {
  const child = Bun.spawn([process.execPath, fileURLToPath(new URL("../volume/index.ts", import.meta.url))], { stdin: "pipe", stdout: "pipe", stderr: "pipe" });
  const lines = createInterface({ input: Readable.from(child.stdout) })[Symbol.asyncIterator]();
  try {
    child.stdin.write(JSON.stringify({ options: { percent: 40 }, user: "1", guild: "2", channel: "3" }) + "\n");
    assert.deepEqual(JSON.parse((await lines.next()).value!), { tool: "volume", args: { percent: 40 } });
    child.stdin.write(JSON.stringify({ result: "Set to 40%.\nReady." }) + "\n");
    assert.deepEqual(JSON.parse((await lines.next()).value!), { tool: "reply", args: { text: "Set to 40%.\nReady." } });
    child.stdin.write(JSON.stringify({ result: "Replied." }) + "\n");
    child.stdin.end();
    assert.equal(await child.exited, 0, await new Response(child.stderr).text());
  } finally { child.kill(); }
})), 5000);

test("host failures fail the mod instead of reporting success", async () => {
  const child = Bun.spawn([process.execPath, fileURLToPath(new URL("../volume/index.ts", import.meta.url))], { stdin: "pipe", stdout: "pipe", stderr: "pipe" });
  const lines = createInterface({ input: Readable.from(child.stdout) })[Symbol.asyncIterator]();
  try {
    child.stdin.write(JSON.stringify({ options: { percent: 40 }, user: "1", guild: "2", channel: "3" }) + "\n");
    assert.equal(JSON.parse((await lines.next()).value!).tool, "volume");
    child.stdin.write(JSON.stringify({ result: "Failed: No voice channel." }) + "\n");
    child.stdin.end();
    assert.equal((await lines.next()).done, true);
    assert.notEqual(await child.exited, 0);
    assert.match(await new Response(child.stderr).text(), /No voice channel/);
  } finally { child.kill(); }
}, 5000);
