<script lang="ts">
  import { invoke, type CodexModel, type Provider } from "./invoke";

  let { signedIn = $bindable(), codex = $bindable(), provider = $bindable(), report, message }: {
    signedIn: boolean; codex: CodexModel[]; provider: Provider; report: (error: unknown) => void; message: string;
  } = $props();

  let baseUrl = $state(provider.baseUrl);
  let key = $state("");
  let busy = $state(false);
  let note = $state("");
  $effect(() => { baseUrl = provider.baseUrl; });

  async function run(label: string, action: () => Promise<void>) {
    busy = true; note = label;
    try { await action(); } catch (error) { report(error); note = ""; } finally { busy = false; }
  }

  const signIn = () => run("Finish signing in to ChatGPT in your browser…", async () => {
    signedIn = await invoke("codex_sign_in");
    codex = await invoke("codex_models");
    note = "Signed in. Model choices come from your ChatGPT plan.";
  });
  const signOut = () => run("", async () => { signedIn = await invoke("codex_sign_out"); codex = []; note = "Signed out."; });
  const saveEndpoint = (clearKey = false) => run("", async () => {
    provider = await invoke("save_provider", { baseUrl, apiKey: key, clearKey });
    key = "";
    note = "Saved. Press Refresh models to load its model list.";
  });
  // Refresh feeds the dropdowns on the Realtime and Worker tabs for whichever providers are set up.
  const refresh = () => run("Asking the provider for its models…", async () => {
    if (signedIn) codex = await invoke("codex_models");
    if (provider.baseUrl) provider = await invoke("refresh_provider_models");
    note = provider.models.length ? `${provider.models.length} models found${provider.efforts.length ? " with reasoning levels" : "; the provider lists no reasoning levels, so those stay text fields"}.` : "Refreshed. The provider lists no models, so model fields stay text inputs.";
  });
</script>

<section aria-label="ChatGPT">
  <h2>ChatGPT</h2>
  {#if signedIn}
    <p class="hint" role="status">Signed in with ChatGPT.</p>
    <div class="actions"><button type="button" class="secondary" disabled={busy} onclick={signOut}>Sign out</button></div>
  {:else}
    <p class="hint">Use your ChatGPT Codex subscription.</p>
    <div class="actions"><button type="button" disabled={busy} onclick={signIn}>Sign in with ChatGPT</button></div>
  {/if}
</section>

<section aria-label="OpenAI-compatible provider">
  <h2>OpenAI-compatible provider</h2>
  <form onsubmit={(event) => { event.preventDefault(); void saveEndpoint(); }}>
    <label for="base-url">Base URL</label>
    <input id="base-url" bind:value={baseUrl} placeholder="https://api.openai.com/v1" autocomplete="off" required />
    <label for="api-key">API key <span>(optional for local servers)</span></label>
    <input id="api-key" type="password" bind:value={key} placeholder={provider.hasKey ? "Stored — type to replace" : "sk-…"} autocomplete="off" />
    <div class="actions">
      <button type="submit" disabled={busy}>Save</button>
      {#if provider.hasKey}<button type="button" class="secondary" disabled={busy} onclick={() => saveEndpoint(true)}>Remove key</button>{/if}
    </div>
  </form>
</section>

<section aria-label="Models">
  <h2>Models</h2>
  <p class="hint">Reads the model lists the Realtime and Worker tabs pick from.</p>
  <div class="actions"><button type="button" disabled={busy || (!signedIn && !provider.baseUrl)} onclick={refresh}>Refresh models</button></div>
  <p class="hint" role="status" aria-live="polite">{note || message}</p>
</section>
