<script lang="ts">
  import ProviderPanel from "./ProviderPanel.svelte";
  import SpeechPanel from "./SpeechPanel.svelte";
  import RolePanel from "./RolePanel.svelte";
  import { invoke, type CodexModel, type Provider, type Roles } from "./invoke";

  const tabs = [["realtime", "Realtime"], ["worker", "Worker"], ["provider", "Provider"], ["stt", "STT"], ["tts", "TTS"]] as const;
  const empty = () => ({ provider: "", model: "", effort: "medium", fast: false, packages: [] });

  let tab = $state<(typeof tabs)[number][0]>("realtime");
  let signedIn = $state(false);
  let codex = $state<CodexModel[]>([]);
  let provider = $state<Provider>({ baseUrl: "", models: [], efforts: {}, hasKey: false });
  let roles = $state<Roles>({ realtime: empty(), worker: empty() });
  let message = $state("");

  const report = (error: unknown) => (message = error instanceof Error ? error.message : String(error));

  async function load() {
    try {
      [roles, signedIn, provider] = await Promise.all([invoke("get_roles"), invoke("codex_signed_in"), invoke("get_provider")]);
      if (signedIn) codex = await invoke("codex_models");
    } catch (error) { report(error); }
  }

  async function save() {
    try { roles = await invoke("save_roles", { roles }); message = "Saved. Changes apply the next time Wallis starts."; } catch (error) { report(error); }
  }

  void load();
</script>

<h1>Inference</h1>
<div class="tabs" role="tablist">
  {#each tabs as [id, label] (id)}
    <button type="button" role="tab" aria-selected={tab === id} onclick={() => (tab = id)}>{label}</button>
  {/each}
</div>

{#if tab === "provider"}
  <ProviderPanel bind:signedIn bind:codex bind:provider {report} {message} />
{:else if tab === "stt" || tab === "tts"}
  <SpeechPanel mode={tab} />
{:else}
  <RolePanel
    title={tab === "realtime" ? "Realtime — conversation, with memory" : "Worker — background tasks, sandboxed, no memory"}
    bind:role={roles[tab]} {signedIn} {codex} {provider} {save} {message} />
{/if}
