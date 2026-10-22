<script lang="ts">
  import { defaultConfig, invoke, type VoiceOption, type WallisConfig } from "./invoke";

  let { mode }: { mode: "stt" | "tts" } = $props();
  let config = $state<WallisConfig>(defaultConfig());
  let sttOptions = $state<VoiceOption[]>([]);
  let ttsOptions = $state<VoiceOption[]>([]);
  let note = $state("");
  let ready = $state(false);

  async function load() {
    try {
      [config, [sttOptions, ttsOptions]] = await Promise.all([invoke("get_config"), invoke("get_voice_options")]);
      ready = true;
    } catch (error) { note = String(error); }
  }

  async function save(event: SubmitEvent) {
    event.preventDefault();
    try {
      const current = await invoke("get_config");
      const speech = mode === "stt" ? { sttModel: config.sttModel, sttThreads: config.sttThreads } : { ttsVoice: config.ttsVoice, ttsSpeed: config.ttsSpeed };
      config = await invoke("save_config", { config: { ...current, ...speech } });
      note = "Saved. Restart Wallis to apply speech changes.";
    } catch (error) { note = String(error); }
  }

  void load();
</script>

<section aria-labelledby="speech-title">
  <h2 id="speech-title">{mode === "stt" ? "Speech recognition" : "Text to speech"}</h2>
  <form onsubmit={save}>
    {#if mode === "stt"}
      <label for="stt">Speech recognition</label>
      <select id="stt" bind:value={config.sttModel}>{#each sttOptions as option (option.id)}<option value={option.id}>{option.label} · {option.megabytes} MB</option>{/each}</select>
      <label for="threads">Recognition threads</label>
      <input id="threads" type="number" min="1" max="64" bind:value={config.sttThreads} />
    {:else}
      <label for="tts">Voice <span>(Wallis picks his own emotion)</span></label>
      <select id="tts" bind:value={config.ttsVoice}>{#each ttsOptions as option (option.id)}<option value={option.id}>{option.label}</option>{/each}</select>
      <label for="speed">Speaking speed <span>(lower is faster; also changes pitch)</span></label>
      <input id="speed" type="number" min="0.5" max="2" step="0.05" bind:value={config.ttsSpeed} />
    {/if}
    <button type="submit" disabled={!ready}>Save</button>
  </form>
  <p class="hint" role="status" aria-live="polite">{note}</p>
</section>
