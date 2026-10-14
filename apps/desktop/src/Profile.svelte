<script lang="ts">
  import { invoke, type Profile } from "./invoke";

  let { ready, onChange }: { ready: boolean; onChange: () => void } = $props();

  let profile = $state<Profile | null>(null);
  let hint = $state("");
  let busy = $state(false);
  let note = $state("");

  const load = () => invoke("get_profile").then((p) => (profile = p)).catch(() => (profile = null));

  async function generate() {
    busy = true; note = "Wallis is inventing himself…";
    try { profile = await invoke("generate_wallis", { hint }); onChange(); note = ""; }
    catch (error) { note = error instanceof Error ? error.message : String(error); }
    finally { busy = false; }
  }

  async function lobotomize() {
    if (!confirm("Lobotomize Wallis? His profile and all of his memories are erased for good, and he goes offline.")) return;
    busy = true;
    try { await invoke("lobotomize_wallis"); profile = null; hint = ""; note = ""; onChange(); }
    catch (error) { note = error instanceof Error ? error.message : String(error); }
    finally { busy = false; }
  }

  void load();
</script>

<h1>Profile</h1>
{#if profile}
  <section aria-label="Profile">
    <p class="profile-name">{profile.name}{profile.age ? ` · ${profile.age}` : ""}{profile.pronouns ? ` · ${profile.pronouns}` : ""}</p>
    {#if profile.tagline}<p class="tagline">“{profile.tagline}”</p>{/if}
    <p>{profile.bio}</p>
    {#if profile.interests.length}<ul class="tags">{#each profile.interests as interest (interest)}<li>{interest}</li>{/each}</ul>{/if}
    <dl>
      {#each [["Location", profile.location], ["Does", profile.occupation], ["Looking for", profile.lookingFor], ["Fun fact", profile.funFact]] as [label, value] (label)}
        {#if value}<div class="field"><dt>{label}</dt><dd>{value}</dd></div>{/if}
      {/each}
    </dl>
    <p class="hint">This profile is Wallis' own. Only he can change it, and only when he decides to.</p>
    <div class="actions"><button type="button" class="secondary" disabled={busy} onclick={lobotomize}>Lobotomize Wallis</button></div>
    <p class="hint" role="status" aria-live="polite">{note}</p>
  </section>
{:else}
  <section aria-label="Create Wallis">
    <h2>Meet Wallis</h2>
    <p>The realtime model invents Wallis. Afterwards only he can change his profile.</p>
    <label for="hint">Personality preferences <span>(optional)</span></label>
    <textarea id="hint" rows="4" bind:value={hint} maxlength="2000" placeholder="e.g. a dry-witted night-shift baker who loves old sci-fi" disabled={busy}></textarea>
    <p class="hint">Optional preferences. The same generator takes them into account and still chooses the details itself.</p>
    <div class="actions"><button type="button" disabled={busy || !ready} onclick={generate}>Generate Wallis</button></div>
    {#if !ready}<p class="hint">Pick a realtime model on the Inference page first.</p>{/if}
    <p class="hint" role="status" aria-live="polite">{note}</p>
  </section>
{/if}
