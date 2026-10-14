<script lang="ts">
  import type { CodexModel, Provider, RoleConfig } from "./invoke";
  import { CODEX_PROVIDER, CUSTOM_PROVIDER, chooseModel, packageError, parsePackages, selectedModel } from "./roles";

  let { title, role = $bindable(), signedIn, codex, provider, save, message }: {
    title: string; role: RoleConfig; signedIn: boolean; codex: CodexModel[]; provider: Provider; save: () => void; message: string;
  } = $props();

  let packages = $state(role.packages.join("\n"));
  let problem = $derived(packageError(parsePackages(packages)));
  const entry = $derived(selectedModel(role, codex));
  const custom = $derived(role.provider === CUSTOM_PROVIDER);

  function pickSource(source: string) {
    if (source === CODEX_PROVIDER && codex[0]) role = chooseModel(role, codex[0]);
    else if (source === CUSTOM_PROVIDER) role = { ...role, provider: CUSTOM_PROVIDER, model: provider.models[0] ?? "", effort: provider.efforts[0] ?? role.effort, fast: false };
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

  {#if entry}
    <label for="model">Model</label>
    <select id="model" value={entry.id} onchange={(event) => { const next = codex.find((m) => m.id === event.currentTarget.value); if (next) role = chooseModel(role, next); }}>
      {#each codex as m (m.id)}<option value={m.id}>{m.label}</option>{/each}
    </select>
    {#if entry.description}<p class="hint">{entry.description}</p>{/if}
    <label for="effort">Reasoning level</label>
    <select id="effort" bind:value={role.effort}>{#each entry.efforts as level (level)}<option value={level}>{level}</option>{/each}</select>
    <label class="check"><input type="checkbox" bind:checked={role.fast} disabled={!entry.supportsFast} /> Fast mode{entry.supportsFast ? "" : " (not offered for this model)"}</label>
  {:else if custom}
    <label for="model">Model</label>
    {#if provider.models.length}
      <select id="model" bind:value={role.model}>{#each provider.models as id (id)}<option value={id}>{id}</option>{/each}</select>
    {:else}
      <input id="model" bind:value={role.model} placeholder="model id" autocomplete="off" />
      <p class="hint">This provider did not list its models. Type the id, or press Refresh models on the Provider tab.</p>
    {/if}
    <label for="effort">Reasoning level</label>
    {#if provider.efforts.length}
      <select id="effort" bind:value={role.effort}>{#each provider.efforts as level (level)}<option value={level}>{level}</option>{/each}</select>
    {:else}
      <input id="effort" bind:value={role.effort} placeholder="medium" autocomplete="off" />
    {/if}
  {/if}

  <label for="packages">Pi packages <span>(one per line: npm:, git: or https://)</span></label>
  <textarea id="packages" rows="3" bind:value={packages} placeholder="npm:@scope/package" spellcheck="false"></textarea>
  <p class="hint">Installed with <code>pi install</code> when Wallis starts. See <a href="https://pi.dev/docs/latest/packages" target="_blank" rel="noreferrer">Pi packages</a>. Packages run code, so add only ones you trust.</p>
  {#if problem}<p class="hint bad" role="alert">{problem}</p>{/if}

  <div class="actions"><button type="button" disabled={!!problem} onclick={submit}>Save</button></div>
  <p class="hint" role="status" aria-live="polite">{message}</p>
</section>
