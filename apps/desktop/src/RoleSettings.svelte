<script lang="ts">
  import { invoke, type CodexModel, type RoleConfig, type Roles } from "./invoke";
  import { CODEX_PROVIDER, MANUAL_EFFORTS, chooseModel, selectedModel } from "./roles";

  const titles = { realtime: "Realtime — conversation, with memory", worker: "Worker — background tasks, sandboxed, no memory" } as const;
  const empty = (): RoleConfig => ({ provider: "", model: "", effort: "medium", fast: false });

  let signedIn = $state(false);
  let models = $state<CodexModel[]>([]);
  let roles = $state<Roles>({ realtime: empty(), worker: empty() });
  let busy = $state(false);
  let message = $state("");

  const report = (error: unknown) => (message = error instanceof Error ? error.message : String(error));

  async function loadModels() {
    models = await invoke("codex_models");
  }

  async function load() {
    try {
      [roles, signedIn] = await Promise.all([invoke("get_roles"), invoke("codex_signed_in")]);
      if (signedIn) await loadModels();
    } catch (error) { report(error); }
  }

  async function signIn() {
    busy = true; message = "Finish signing in to ChatGPT in your browser…";
    try {
      signedIn = await invoke("codex_sign_in");
      await loadModels();
      message = "Signed in. Model choices come from your ChatGPT plan.";
    } catch (error) { report(error); } finally { busy = false; }
  }

  async function signOut() {
    try { signedIn = await invoke("codex_sign_out"); models = []; message = "Signed out."; } catch (error) { report(error); }
  }

  async function save() {
    try { roles = await invoke("save_roles", { roles }); message = "Saved. Changes apply to the next conversation."; } catch (error) { report(error); }
  }

  const useCodex = (name: keyof Roles) => {
    const [first] = models;
    if (first) roles[name] = chooseModel(roles[name], first);
  };

  void load();
</script>

<section aria-labelledby="roles-title">
  <h2 id="roles-title">AI models</h2>
  <div class="actions">
    {#if signedIn}
      <p class="hint" role="status">Signed in with ChatGPT.</p>
      <button type="button" class="secondary" onclick={signOut}>Sign out</button>
      <button type="button" class="secondary" onclick={() => loadModels().catch(report)}>Refresh models</button>
    {:else}
      <button type="button" disabled={busy} onclick={signIn}>Sign in with ChatGPT</button>
      <p class="hint">Use your ChatGPT Codex subscription, or configure a provider and model by hand below.</p>
    {/if}
  </div>

  {#each ["realtime", "worker"] as const as name (name)}
    {@const role = roles[name]}
    {@const model = selectedModel(role, models)}
    <fieldset>
      <legend>{titles[name]}</legend>
      {#if signedIn && models.length}
        <label for="{name}-source">Provider</label>
        <select id="{name}-source" value={role.provider === CODEX_PROVIDER ? CODEX_PROVIDER : "manual"}
          onchange={(event) => event.currentTarget.value === CODEX_PROVIDER ? useCodex(name) : (roles[name].provider = "")}>
          <option value={CODEX_PROVIDER}>ChatGPT (Codex subscription)</option>
          <option value="manual">Other provider</option>
        </select>
      {/if}

      {#if model}
        <label for="{name}-model">Model</label>
        <select id="{name}-model" value={model.id} onchange={(event) => { const next = models.find((entry) => entry.id === event.currentTarget.value); if (next) roles[name] = chooseModel(role, next); }}>
          {#each models as entry (entry.id)}<option value={entry.id}>{entry.label}</option>{/each}
        </select>
        {#if model.description}<p class="hint">{model.description}</p>{/if}
        <label for="{name}-effort">Reasoning level</label>
        <select id="{name}-effort" bind:value={roles[name].effort}>
          {#each model.efforts as level (level)}<option value={level}>{level}</option>{/each}
        </select>
        <label class="check"><input type="checkbox" bind:checked={roles[name].fast} disabled={!model.supportsFast} /> Fast mode{model.supportsFast ? "" : " (not offered for this model)"}</label>
      {:else}
        <label for="{name}-provider">Provider</label>
        <input id="{name}-provider" bind:value={roles[name].provider} placeholder="openrouter" autocomplete="off" />
        <label for="{name}-manual-model">Model</label>
        <input id="{name}-manual-model" bind:value={roles[name].model} placeholder="anthropic/claude-opus-5" autocomplete="off" />
        <label for="{name}-manual-effort">Reasoning level</label>
        <select id="{name}-manual-effort" bind:value={roles[name].effort}>
          {#each MANUAL_EFFORTS as level (level)}<option value={level}>{level}</option>{/each}
        </select>
        <p class="hint">Provider API keys are read from Pi's usual environment variables.</p>
      {/if}
    </fieldset>
  {/each}

  <div class="actions"><button type="button" onclick={save}>Save models</button></div>
  <p class="hint" role="status" aria-live="polite">{message}</p>
</section>
