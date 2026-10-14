<script lang="ts">
  import { validateConfig } from "./config";
  import { defaultConfig, invoke, type Job, type RuntimeStatus, type Stats, type UpdateCheck, type VoiceOption, type WallisConfig } from "./invoke";

  let { runtime = $bindable(), onboarded, refreshConfig }: { runtime: RuntimeStatus; onboarded: boolean; refreshConfig: () => void } = $props();

  const message = (error: unknown) => (error instanceof Error ? error.message : String(error));
  const seconds = (ms: number) => (ms ? `${(ms / 1000).toFixed(1)} s` : "—");

  let config = $state<WallisConfig>(defaultConfig());
  let hasToken = $state(false);
  let token = $state("");
  let sttOptions = $state<VoiceOption[]>([]);
  let ttsOptions = $state<VoiceOption[]>([]);
  let jobs = $state<Job[]>([]);
  let stats = $state<Stats | null>(null);
  let progress = $state("");
  let busy = $state(false);
  let note = $state("");
  let update = $state<UpdateCheck | null>(null);

  async function poll() {
    runtime = await invoke("get_runtime_status");
    [jobs, stats] = await Promise.all([invoke("get_jobs").catch(() => []), invoke("get_stats").catch(() => null)]);
  }

  async function load() {
    try {
      [config, hasToken, [sttOptions, ttsOptions]] = await Promise.all([invoke("get_config"), invoke("get_secret_status").then((s) => s.hasToken), invoke("get_voice_options")]);
      await poll();
    } catch (error) { note = message(error); }
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

  async function save(event: SubmitEvent) {
    event.preventDefault();
    const clean = { ...config, discordReadChannels: config.discordReadChannels.replace(/\s/g, ""), discordWriteChannels: config.discordWriteChannels.replace(/\s/g, ""), discordVoiceTextChannel: config.discordVoiceTextChannel.trim() };
    const problem = validateConfig(clean);
    if (problem) { note = problem; return; }
    try {
      config = await invoke("save_config", { config: clean });
      if (token) hasToken = (await invoke("save_discord_token", { token })).hasToken;
      token = "";
      note = "Saved. Restart Wallis to apply Discord and voice changes.";
      refreshConfig();
    } catch (error) { note = message(error); }
  }

  async function retry(id: number) { await invoke("retry_job", { id }).catch((error) => (note = message(error))); await poll(); }

  void load();
  setInterval(() => { if (!busy) void poll().catch(() => {}); }, 5000);
</script>

<h1>Home</h1>
<section aria-labelledby="run-title">
  <h2 id="run-title">Status</h2>
  <p role="status" aria-live="polite">{progress || note || runtime.message}</p>
  <div class="actions">
    <button type="button" disabled={busy || runtime.state === "running" || !onboarded || !hasToken} onclick={start}>Bring online</button>
    <button type="button" class="secondary" disabled={runtime.state !== "running"} onclick={stop}>Take offline</button>
  </div>
  {#if !hasToken}<p class="hint">Add your Discord bot token below to start.</p>{:else if !onboarded}<p class="hint">Choose models on the Inference page, then create Wallis on the Profile page.</p>{/if}
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

<section aria-labelledby="discord-title">
  <h2 id="discord-title">Discord and voice</h2>
  <form onsubmit={save}>
    <label for="token">Bot token</label>
    <input id="token" type="password" bind:value={token} placeholder={hasToken ? "Stored securely — type to replace" : "Paste the bot token"} autocomplete="off" />
    <label for="app-id">Application ID <span>(optional)</span></label>
    <input id="app-id" bind:value={config.discordApplicationId} autocomplete="off" />
    <label for="users">Allowed user IDs <span>(comma separated, or *)</span></label>
    <input id="users" bind:value={config.discordAllowedUsers} autocomplete="off" required />
    <label for="read">Channels Wallis reads <span>(IDs)</span></label>
    <input id="read" bind:value={config.discordReadChannels} autocomplete="off" />
    <label for="write">Channels Wallis may write to <span>(IDs)</span></label>
    <input id="write" bind:value={config.discordWriteChannels} autocomplete="off" />
    <label for="voice-text">Voice call text channel <span>(one of the write channels)</span></label>
    <input id="voice-text" bind:value={config.discordVoiceTextChannel} autocomplete="off" />
    <label for="voice-mode">Voice participation</label>
    <select id="voice-mode" bind:value={config.voiceParticipation}><option value="natural">Join in naturally</option><option value="mentions">Only when addressed</option></select>
    <label for="text-mode">Text participation</label>
    <select id="text-mode" bind:value={config.textParticipation}><option value="natural">Join in naturally</option><option value="mentions">Only when mentioned</option></select>
    <label for="cooldown">Seconds between unprompted contributions</label>
    <input id="cooldown" type="number" min="5" max="300" bind:value={config.participationCooldownSecs} />
    <label for="stt">Speech recognition</label>
    <select id="stt" bind:value={config.sttModel}>{#each sttOptions as option (option.id)}<option value={option.id}>{option.label} · {option.megabytes} MB</option>{/each}</select>
    <label for="threads">Recognition threads</label>
    <input id="threads" type="number" min="1" max="64" bind:value={config.sttThreads} />
    <label for="tts">Voice</label>
    <select id="tts" bind:value={config.ttsVoice}>{#each ttsOptions as option (option.id)}<option value={option.id}>{option.label} · {option.megabytes} MB</option>{/each}</select>
    <label for="speed">Speaking speed <span>(lower is faster; also changes pitch)</span></label>
    <input id="speed" type="number" min="0.5" max="2" step="0.05" bind:value={config.ttsSpeed} />
    <label class="check"><input type="checkbox" bind:checked={config.autoStart} /> Start Wallis when the app opens</label>
    <button type="submit">Save</button>
  </form>
</section>
