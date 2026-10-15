<script lang="ts">
  import { invoke, type Mod } from "./invoke";
  let mods = $state<Mod[]>([]);
  let busy = $state(false);
  let error = $state("");
  let deleting = $state("");
  async function load() {
    busy = true;
    try { mods = await invoke("get_mods"); error = ""; }
    catch (e) { error = String(e); }
    finally { busy = false; }
  }
  async function change(mod: Mod, remove = false) {
    busy = true;
    try {
      mods = remove ? await invoke("delete_mod", { name: mod.name }) : await invoke("set_mod_enabled", { name: mod.name, enabled: !mod.enabled });
      error = ""; deleting = "";
    } catch (e) { error = String(e); }
    finally { busy = false; }
  }
  void load();
</script>
<h1>Commands</h1>
<p class="hint">Ask Wallis to learn a feature. His worker writes a command here, and it appears in Discord after the job finishes.</p>
<p class="hint">Commands run inside Wallis' Linux environment. Workspace limit: 3 GiB. Music downloads are deleted after playback.</p>
{#if error}<p role="alert">{error}</p>{/if}
<button type="button" class="secondary" disabled={busy} onclick={load}>Refresh</button>
{#each mods as mod (mod.name)}
  <section aria-labelledby={`command-${mod.name}`}>
    <h2 id={`command-${mod.name}`}>/{mod.name}</h2>
    <p>{mod.description}</p>
    {#if mod.options.length}<p class="hint">Options: {mod.options.map((o) => `${o.name}${o.required ? " (required)" : ""}`).join(", ")}</p>{/if}
    <div class="actions">
      <button type="button" class="secondary" aria-pressed={mod.enabled} disabled={busy} onclick={() => change(mod)}>{mod.enabled ? "Enabled" : "Disabled"}</button>
      <button type="button" class="secondary" disabled={busy} onclick={() => deleting = mod.name}>Delete</button>
    </div>
    {#if deleting === mod.name}
      <p>Delete /{mod.name} and its code? This cannot be undone.</p>
      <div class="actions">
        <button type="button" disabled={busy} onclick={() => change(mod, true)}>Delete permanently</button>
        <button type="button" class="secondary" disabled={busy} onclick={() => deleting = ""}>Cancel</button>
      </div>
    {/if}
  </section>
{/each}
{#if busy}<p role="status">Loading commands…</p>{/if}
