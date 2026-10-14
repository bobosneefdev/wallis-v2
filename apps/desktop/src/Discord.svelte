<script lang="ts">
  import { validateConfig } from "./config";
  import { defaultConfig, invoke, type VoiceOption, type WallisConfig } from "./invoke";

  let { refresh }: { refresh: () => void } = $props();

  const message = (error: unknown) => (error instanceof Error ? error.message : String(error));

  let config = $state<WallisConfig>(defaultConfig());
  let hasToken = $state(false);
  let token = $state("");
  let sttOptions = $state<VoiceOption[]>([]);
  let ttsOptions = $state<VoiceOption[]>([]);
  let note = $state("");

  async function load() {
    try { [config, hasToken, [sttOptions, ttsOptions]] = await Promise.all([invoke("get_config"), invoke("get_secret_status").then((s) => s.hasToken), invoke("get_voice_options")]); }
    catch (error) { note = message(error); }
  }

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
      refresh();
    } catch (error) { note = message(error); }
  }

  void load();
</script>

<h1>Discord</h1>
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
  <p class="hint" role="status" aria-live="polite">{note}</p>
</section>
