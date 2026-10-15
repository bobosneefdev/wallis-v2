<script lang="ts">
  import { invoke, type AuditEvent, type Job, type RuntimeStatus, type Stats, type UpdateCheck } from "./invoke";

  let { runtime = $bindable(), hasProfile, hasToken }: { runtime: RuntimeStatus; hasProfile: boolean; hasToken: boolean } = $props();

  const message = (error: unknown) => (error instanceof Error ? error.message : String(error));
  const seconds = (ms: number) => (ms ? `${(ms / 1000).toFixed(1)} s` : "—");

  let jobs = $state<Job[]>([]);
  let stats = $state<Stats | null>(null);
  let audit = $state<AuditEvent[]>([]);
  const auditLabel = { heard: "Heard", typed: "Typed", prompt: "Prompt", steer: "Steer", delegate: "Delegated" };
  let progress = $state("");
  let busy = $state(false);
  let note = $state("");
  let update = $state<UpdateCheck | null>(null);

  async function poll() {
    runtime = await invoke("get_runtime_status");
    [jobs, stats, audit] = await Promise.all([invoke("get_jobs").catch(() => []), invoke("get_stats").catch(() => null), invoke("get_audit_log").catch(() => [])]);
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

<section aria-labelledby="audit-title">
  <h2 id="audit-title">Audit Log</h2>
  {#if audit.length}
    <ol class="jobs audit">
      {#each audit.toReversed() as event}
        <li><time>{new Date(event.atMs).toLocaleTimeString()}</time> <span class="pill" class:ok={event.kind === "delegate"}>{auditLabel[event.kind]}</span> {event.text}</li>
      {/each}
    </ol>
  {:else}<p class="hint">The latest 256 messages, agent prompts, steers and delegations appear here while Wallis is online.</p>{/if}
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
