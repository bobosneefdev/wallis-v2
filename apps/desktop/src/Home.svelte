<script lang="ts">
  import { tick } from "svelte";
  import { invoke, type Job, type LogLine, type RuntimeStatus, type Stats, type UpdateCheck } from "./invoke";

  let { runtime = $bindable(), hasProfile, hasToken }: { runtime: RuntimeStatus; hasProfile: boolean; hasToken: boolean } = $props();

  const message = (error: unknown) => (error instanceof Error ? error.message : String(error));
  const seconds = (ms: number) => (ms ? `${(ms / 1000).toFixed(1)} s` : "—");

  let jobs = $state<Job[]>([]);
  let stats = $state<Stats | null>(null);
  // The live log: lines are fetched once and appended, so it reads like a terminal. Scrolled up = paused following.
  let lines = $state<LogLine[]>([]);
  let since = 0;
  let filter = $state("");
  let follow = $state(true);
  let logBox = $state<HTMLPreElement>();
  const shown = $derived(filter ? lines.filter((line) => `${line.tag} ${line.text}`.toLowerCase().includes(filter.toLowerCase())) : lines);
  const clock = (ms: number) => { const d = new Date(ms); return `${d.toLocaleTimeString([], { hour12: false })}.${String(d.getMilliseconds()).padStart(3, "0")}`; };
  const format = (line: LogLine) => `${clock(line.atMs)} ${line.tag.padEnd(7)} ${line.text}`;

  async function pollLog() {
    const tail = await invoke("get_log", { since });
    if (tail.next < since) lines = []; // the runtime restarted and counts from zero again
    since = tail.next;
    if (!tail.lines.length) return;
    lines = [...lines, ...tail.lines].slice(-2000);
    if (follow) { await tick(); logBox?.scrollTo({ top: logBox.scrollHeight }); }
  }
  const scrolled = () => { if (logBox) follow = logBox.scrollHeight - logBox.scrollTop - logBox.clientHeight < 24; };
  const copyLog = () => navigator.clipboard.writeText(shown.map(format).join("\n")).then(() => (note = "Log copied."), (error) => (note = message(error)));
  let progress = $state("");
  let busy = $state(false);
  let note = $state("");
  let update = $state<UpdateCheck | null>(null);

  async function poll() {
    runtime = await invoke("get_runtime_status");
    [jobs, stats] = await Promise.all([invoke("get_jobs").catch(() => []), invoke("get_stats").catch(() => null)]);
  }

  /** Runs a long command while polling Rust for the current install step. */
  async function work<T>(label: string, run: () => Promise<T>): Promise<T | undefined> {
    busy = true; note = label;
    const timer = setInterval(() => invoke("get_progress").then((step) => step && (progress = step)).catch(() => {}), 1000);
    try { return await run(); } catch (error) { note = message(error); } finally { clearInterval(timer); progress = ""; busy = false; }
  }

  const start = async () => { const next = await work("Starting Wallis…", () => invoke("start_wallis")); if (next) { runtime = next; note = ""; } };
  const stop = async () => { runtime = await invoke("stop_wallis"); stats = null; };
  const checkUpdate = async () => { update = (await work("Checking for updates…", () => invoke("check_update"))) ?? null; if (update) note = ""; };
  const installUpdate = () => work("Updating Wallis…", () => invoke("install_update"));

  async function retry(id: number) { await invoke("retry_job", { id }).catch((error) => (note = message(error))); await poll(); }

  void poll().catch((error) => (note = message(error)));
  setInterval(() => { if (!busy) void poll().catch(() => {}); }, 5000);
  setInterval(() => { if (!busy) void pollLog().catch(() => {}); }, 1000);
</script>

<h1>Home</h1>
<section aria-labelledby="run-title">
  <h2 id="run-title">Status</h2>
  <p class="hint">Requires WSL2. First setup downloads the Linux environment. If WSL2 is missing, run <code>wsl --install --no-distribution</code> as administrator, restart Windows, then retry.</p>
  <p role="status" aria-live="polite">{progress || note || runtime.message}</p>
  <div class="actions">
    <button type="button" disabled={busy || runtime.state === "running" || !hasProfile || !hasToken} onclick={start}>Bring online</button>
    <button type="button" class="secondary" disabled={runtime.state !== "running"} onclick={stop}>Take offline</button>
  </div>
  {#if !hasProfile}<p class="hint">Choose models on the Inference page, then generate Wallis on the Profile page.</p>{:else if !hasToken}<p class="hint">Add your Discord bot token on the Discord page to start.</p>{/if}
</section>

<section aria-labelledby="perf-title">
  <h2 id="perf-title">Performance</h2>
  {#if stats}
    <dl class="stats">
      <div class="field"><dt>Replies</dt><dd>{stats.turns}</dd></div>
      <div class="field"><dt>Average reply time</dt><dd>{seconds(stats.averageTurnMs)}</dd></div>
      <div class="field"><dt>Last reply time</dt><dd>{seconds(stats.lastTurnMs)}</dd></div>
      <div class="field"><dt>Steered mid-reply</dt><dd>{stats.steers}</dd></div>
      <div class="field"><dt>Failed turns</dt><dd>{stats.errors}</dd></div>
    </dl>
  {:else}<p class="hint">Stats appear while Wallis is online.</p>{/if}
</section>

<section aria-labelledby="log-title">
  <h2 id="log-title">Live log</h2>
  <p class="hint">Everything Wallis hears and does, with timings: speech recognition, every model call (latency, first token, tokens), tools, speech synthesis, drops, retries and jobs. The full log since start is also kept as <code>runtime.log</code> in the app data folder.</p>
  <div class="log-bar">
    <input type="search" placeholder="Filter, e.g. model, error, tts" aria-label="Filter the log" bind:value={filter} />
    <label class="check"><input type="checkbox" bind:checked={follow} /> Follow</label>
    <button type="button" class="secondary" onclick={copyLog} disabled={!shown.length}>Copy</button>
    <button type="button" class="secondary" onclick={() => (lines = [])} disabled={!lines.length}>Clear</button>
  </div>
  <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
  <pre class="log" tabindex="0" aria-label="Live log" bind:this={logBox} onscroll={scrolled}>{#each shown as line (line.seq)}<span class="log-line" data-tag={line.tag}>{format(line)}</span>
{:else}<span class="log-empty">{runtime.state === "running" ? "Waiting for activity…" : "Lines appear here while Wallis is online."}</span>{/each}</pre>
</section>

{#if jobs.length}
  <section aria-labelledby="jobs-title">
    <h2 id="jobs-title">Background jobs</h2>
    <ul class="jobs">
      {#each jobs.slice(-10).reverse() as job (job.id)}
        <li><span class="pill" class:ok={job.status === "done"}>{job.status}</span> {job.task.slice(0, 120)}
          {#if job.status === "failed"}<button type="button" class="secondary" onclick={() => retry(job.id)}>Retry</button>{/if}</li>
      {/each}
    </ul>
  </section>
{/if}

<section aria-labelledby="update-title">
  <h2 id="update-title">Updates</h2>
  <p class="hint">{#if !update}Wallis updates itself from the latest GitHub release.{:else if update.available}Wallis {update.latest} is available (you have {update.current}).{:else}Wallis {update.current} is up to date.{/if}</p>
  <div class="actions">
    <button type="button" class="secondary" disabled={busy} onclick={checkUpdate}>Check for updates</button>
    {#if update?.available}<button type="button" disabled={busy} onclick={installUpdate}>Update and restart</button>{/if}
  </div>
</section>
