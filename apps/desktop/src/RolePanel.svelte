<script lang="ts">
  import type { CodexModel, Provider, RoleConfig } from "./invoke";
  import { CODEX_PROVIDER, CUSTOM_PROVIDER, chooseCustomModel, chooseModel, packageError, parsePackages, selectedModel } from "./roles";

  let { title, role = $bindable(), signedIn, codex, provider, save, message }: {
    title: string; role: RoleConfig; signedIn: boolean; codex: CodexModel[]; provider: Provider; save: () => void; message: string;
  } = $props();

  let packages = $state(role.packages.join("\n"));
  let problem = $derived(packageError(parsePackages(packages)));
  const entry = $derived(selectedModel(role, codex));
  const isCodex = $derived(role.provider === CODEX_PROVIDER);
  const models = $derived(isCodex ? codex : provider.models.map((id) => ({ id, label: id })));
  // Reasoning levels offered by the chosen model; null means the provider does not expose them (text field).
  const levels = $derived(isCodex ? entry?.efforts ?? [] : provider.efforts[role.model] ?? null);

  // The model and level start empty so nothing is chosen on the user's behalf.
  function pickSource(source: string) {
    if (source !== role.provider) role = { ...role, provider: source, model: "", effort: "", fast: false };
  }

  function pickModel(id: string) {
    const next = codex.find((m) => m.id === id);
    role = isCodex && next ? chooseModel(role, next) : chooseCustomModel(role, id, provider.efforts);
  }

  const submit = () => { if (!problem) { role.packages = parsePackages(packages); save(); } };
</script>

<section aria-label={title}>
  <h2>{title}</h2>
  {#if !signedIn && !provider.baseUrl}<p class="hint">Set up a provider on the Provider tab first.</p>{/if}

  <label for="source">Provider</label>
  <select id="source" value={role.provider} onchange={(event) => pickSource(event.currentTarget.value)}>
    <option value="" disabled>Choose a provider</option>
    {#if signedIn}<option value={CODEX_PROVIDER}>ChatGPT (Codex subscription)</option>{/if}
    {#if provider.baseUrl}<option value={CUSTOM_PROVIDER}>OpenAI-compatible ({provider.baseUrl})</option>{/if}
  </select>

  {#if role.provider}
    <label for="model">Model</label>
    {#if isCodex || provider.models.length}
      <select id="model" value={role.model} onchange={(event) => pickModel(event.currentTarget.value)}>
        <option value="" disabled>Choose a model</option>
        {#each models as m (m.id)}<option value={m.id}>{m.label}</option>{/each}
      </select>
    {:else}
      <input id="model" value={role.model} oninput={(event) => pickModel(event.currentTarget.value)} placeholder="model id" autocomplete="off" />
      <p class="hint">This provider did not list its models. Type the id, or press Refresh models on the Provider tab.</p>
    {/if}
    {#if entry?.description}<p class="hint">{entry.description}</p>{/if}

    <label for="effort">Reasoning level</label>
    {#if levels}
      <select id="effort" bind:value={role.effort} disabled={!role.model}>
        <option value="" disabled>Choose a reasoning level</option>
        {#each levels as level (level)}<option value={level}>{level}</option>{/each}
      </select>
    {:else}
      <input id="effort" bind:value={role.effort} disabled={!role.model} placeholder="medium" autocomplete="off" />
    {/if}
    {#if isCodex}<label class="check"><input type="checkbox" bind:checked={role.fast} disabled={!entry?.supportsFast} /> Fast mode{entry && !entry.supportsFast ? " (not offered for this model)" : ""}</label>{/if}
  {/if}

  <label for="packages">Pi packages <span>(one per line: npm:, git: or https://)</span></label>
  <textarea id="packages" rows="3" bind:value={packages} placeholder="npm:@scope/package" spellcheck="false"></textarea>
  <p class="hint">Installed with <code>pi install</code> when Wallis starts. See <a href="https://pi.dev/docs/latest/packages" target="_blank" rel="noreferrer">Pi packages</a>. Packages run code, so add only ones you trust.</p>
  {#if problem}<p class="hint bad" role="alert">{problem}</p>{/if}

  <div class="actions"><button type="button" disabled={!!problem} onclick={submit}>Save</button></div>
  <p class="hint" role="status" aria-live="polite">{message}</p>
</section>
