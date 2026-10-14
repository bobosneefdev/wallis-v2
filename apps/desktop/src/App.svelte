<script lang="ts">
  import Discord from "./Discord.svelte";
  import Home from "./Home.svelte";
  import Inference from "./Inference.svelte";
  import Profile from "./Profile.svelte";
  import { invoke, type RuntimeStatus } from "./invoke";

  const pages = [["home", "Home"], ["discord", "Discord"], ["profile", "Profile"], ["inference", "Inference"]] as const;

  let page = $state<(typeof pages)[number][0]>("home");
  let runtime = $state<RuntimeStatus>({ state: "stopped", message: "Wallis is stopped." });
  let hasProfile = $state(false);
  let hasToken = $state(false);
  let modelsChosen = $state(false);

  const loadFlags = async () => {
    hasProfile = await invoke("get_profile").then(() => true, () => false);
    hasToken = (await invoke("get_secret_status")).hasToken;
    modelsChosen = (await invoke("get_roles")).realtime.model !== "";
  };
  void loadFlags().catch(() => {});
</script>

<div class="shell">
  <nav aria-label="Wallis">
    <p class="brand">Wallis</p>
    {#each pages as [id, label] (id)}
      <button type="button" class="nav" aria-current={page === id ? "page" : undefined} onclick={() => { page = id; void loadFlags().catch(() => {}); }}>{label}</button>
    {/each}
    <p class="pill" class:ok={runtime.state === "running"} role="status">{runtime.state === "running" ? "online" : "offline"}</p>
  </nav>
  <main>
    <!-- Home stays mounted so its status polling and unsaved form survive page switches. -->
    <div hidden={page !== "home"}><Home bind:runtime {hasProfile} {hasToken} /></div>
    {#if page === "discord"}<Discord refresh={loadFlags} />{/if}
    {#if page === "profile"}<Profile ready={modelsChosen} onChange={() => { void loadFlags().catch(() => {}); runtime = { state: "stopped", message: "Wallis is stopped." }; }} />{/if}
    {#if page === "inference"}<Inference />{/if}
  </main>
</div>
