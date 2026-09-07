import "./styles.css";
import { validateConfig } from "./config";
import { EFFORTS, EFFORT_LABELS, invoke, type Profile, type ProviderOption, type RuntimeStatus, type SecretStatus, type SetupStatus, type WallisConfig } from "./invoke";

const app = document.querySelector<HTMLDivElement>("#app");
if (!app) throw new Error("Wallis mount is missing");

let setup: SetupStatus = { wsl2Available: false, wslVersion: "unknown", message: "Checking WSL2…" };
let runtime: RuntimeStatus = { state: "unavailable", message: "Checking runtime…", discordReady: false, memoryReady: false };
let secret: SecretStatus = { hasToken: false };
let providers: ProviderOption[] = [];

app.innerHTML = `
  <header><div><p class="eyebrow">LOCAL-FIRST DISCORD BOT</p><h1>Wallis</h1><p>One calm place to set up, inspect, and control your local agent.</p></div><p id="state-pill" class="pill">Checking…</p></header>

  <div id="view-setup" hidden>
    <ol class="steps"><li class="now">Import distro</li><li>Choose a model</li><li>Generate Wallis</li></ol>
    <section aria-labelledby="setup-title"><h2 id="setup-title">Setup</h2><p id="setup-message" role="status">${setup.message}</p><form id="install-form">
      <label for="rootfs-archive">Wallis rootfs archive</label><input id="rootfs-archive" name="rootfsArchive" placeholder="C:\\Downloads\\wallis-rootfs.tar" autocomplete="off" required />
      <label for="rootfs-sha256">Expected SHA-256</label><input id="rootfs-sha256" name="rootfsSha256" placeholder="64 hexadecimal characters" autocomplete="off" required />
      <label for="distro-root">Distro install root <span>(optional)</span></label><input id="distro-root" name="distroRoot" placeholder="Default: %LOCALAPPDATA%\\Wallis\\distro" autocomplete="off" />
      <button type="submit">Import verified Wallis distro</button><button id="setup-reset" type="button" class="secondary" hidden>Reset Wallis</button><p id="install-message" role="status" aria-live="polite"></p>
    </form></section>
  </div>

  <div id="view-inference" hidden>
    <ol class="steps"><li class="done" data-view="setup">Import distro</li><li class="now">Choose a model</li><li>Generate Wallis</li></ol>
    <section aria-labelledby="inference-title"><h2 id="inference-title">How Wallis thinks</h2><p>Pick the inference Wallis runs on. You can change it later.</p><form id="inference-form">
      <label for="provider">Provider</label><select id="provider" name="provider"></select>
      <label for="model">Model</label><input id="model" name="model" list="model-options" autocomplete="off" placeholder="anthropic/claude-opus-5" required /><datalist id="model-options"></datalist>
      <label for="effort">Reasoning effort</label><select id="effort" name="reasoningEffort"></select><p class="hint" id="effort-hint"></p>
      <label for="api-key">Provider API key</label><input id="api-key" name="apiKey" type="password" autocomplete="new-password" placeholder="Stored with Windows DPAPI" />
      <p class="hint">The key never leaves this machine and is handed to Hermes over stdin, never written to a file in the distro.</p>
      <button type="submit">Continue</button><p id="inference-message" role="status" aria-live="polite"></p>
    </form></section>
  </div>

  <div id="view-generate" hidden>
    <ol class="steps"><li class="done" data-view="setup">Import distro</li><li class="done" data-view="inference">Choose a model</li><li class="now">Generate Wallis</li></ol>
    <section class="centered" aria-labelledby="generate-title"><h2 id="generate-title">Meet Wallis</h2>
      <p>Wallis invents himself once: a name, a home, a life, a set of opinions — then commits it all to his own memory.</p>
      <p class="hint">This first run also installs his voice: about 400 MB of local speech models and the Discord bot, downloaded into the distro.</p>
      <div class="actions"><button id="generate" type="button">Generate Wallis</button></div>
      <p id="generate-message" role="status" aria-live="polite"></p>
    </section>
  </div>

  <div id="view-loading" hidden>
    <section class="centered loading" aria-live="polite"><div class="spinner" aria-hidden="true"></div>
      <h2>Generating Wallis…</h2><p>This could take a while.</p><p class="hint" id="loading-step">Setting up memory, voice, and the Discord bot.</p>
    </section>
  </div>

  <div id="view-profile" hidden>
    <section id="profile-card" aria-labelledby="profile-title"><h2 id="profile-title">Profile</h2><div id="profile-body"></div>
      <p class="hint">Wallis keeps this current himself, and anyone on the allow-list can pull it up in Discord with <code>/profile</code>.</p>
      <div class="actions"><button id="profile-done" type="button">Continue to dashboard</button><button id="profile-refresh" type="button" class="secondary">Refresh</button></div>
    </section>
  </div>

  <div id="view-dashboard" hidden>
    <nav class="tabs"><button id="change-model" type="button" class="secondary">Change model</button><button id="open-profile" type="button" class="secondary">View Wallis' profile</button></nav>
    <section aria-labelledby="runtime-title"><h2 id="runtime-title">Runtime</h2><p id="runtime-message" role="status">${runtime.message}</p><p class="hint">Wallis runs his own Discord bot in the distro: <code>/join</code> brings him into a voice channel, where he listens, thinks with Hermes, and answers out loud.</p><div id="readiness" class="pills"></div><div class="actions"><button id="start" type="button">Start Wallis</button><button id="stop" type="button" class="secondary">Stop Wallis</button><button id="reset" type="button" class="secondary">Reset Wallis</button></div></section>
    <section aria-labelledby="terminal-title"><h2 id="terminal-title">Terminal</h2><p class="hint">Runs one command at a time inside the Wallis distro as the non-root <code>wallis</code> user.</p>
      <pre id="terminal-output" role="log" aria-live="polite" tabindex="0">Ready.</pre>
      <form id="terminal-form" class="prompt"><span aria-hidden="true">$</span><input id="terminal-command" name="command" placeholder="hermes --help" autocomplete="off" spellcheck="false" required aria-label="Command to run in the Wallis distro" /><button type="submit">Run</button><button id="open-terminal" type="button" class="secondary">Open shell window</button></form></section>
    <section aria-labelledby="config-title"><h2 id="config-title">Configuration</h2><form id="config-form">
      <label for="application-id">Discord application ID <span>(optional until bot setup)</span></label><input id="application-id" name="discordApplicationId" inputmode="numeric" autocomplete="off" />
      <label for="allowed-users">Allowed Discord user IDs</label><input id="allowed-users" name="discordAllowedUsers" inputmode="numeric" placeholder="123456789012345678,198765432109876543" autocomplete="off" required /><p class="hint">Only these users can interact with Wallis. Use numeric Discord user IDs separated by commas.</p>
      <label for="discord-token">Discord bot token</label><input id="discord-token" name="discordToken" type="password" autocomplete="new-password" placeholder="Leave blank to keep the saved token" /><p id="token-status" class="hint" role="status"></p>
      <label class="check"><input id="auto-start" name="autoStart" type="checkbox" /> Start Hermes when Wallis opens</label>
      <p id="inference-summary" class="hint"></p><p class="hint">Memory provider: <strong>Mnemosyne</strong>. Bot tokens are protected with Windows DPAPI and never returned to the UI.</p>
      <button type="submit">Save configuration</button><p id="config-message" role="status" aria-live="polite"></p>
    </form></section>
  </div>`;

const pick = <T extends HTMLElement>(id: string) => document.querySelector<T>(id)!;
const setupMessage = pick("#setup-message");
const installMessage = pick("#install-message");
const runtimeMessage = pick("#runtime-message");
const readiness = pick("#readiness");
const statePill = pick("#state-pill");
const configMessage = pick("#config-message");
const tokenStatus = pick("#token-status");
const inferenceSummary = pick("#inference-summary");
const startButton = pick<HTMLButtonElement>("#start");
const stopButton = pick<HTMLButtonElement>("#stop");
const resetButton = pick<HTMLButtonElement>("#reset");
const installForm = pick<HTMLFormElement>("#install-form");
const form = pick<HTMLFormElement>("#config-form");
const applicationId = pick<HTMLInputElement>("#application-id");
const allowedUsers = pick<HTMLInputElement>("#allowed-users");
const token = pick<HTMLInputElement>("#discord-token");
const autoStart = pick<HTMLInputElement>("#auto-start");
const rootfsArchive = pick<HTMLInputElement>("#rootfs-archive");
const rootfsSha256 = pick<HTMLInputElement>("#rootfs-sha256");
const distroRoot = pick<HTMLInputElement>("#distro-root");
const terminalOutput = pick("#terminal-output");
const terminalForm = pick<HTMLFormElement>("#terminal-form");
const terminalCommand = pick<HTMLInputElement>("#terminal-command");
const openTerminalButton = pick<HTMLButtonElement>("#open-terminal");
const inferenceForm = pick<HTMLFormElement>("#inference-form");
const inferenceMessage = pick("#inference-message");
const providerSelect = pick<HTMLSelectElement>("#provider");
const modelInput = pick<HTMLInputElement>("#model");
const modelOptions = pick("#model-options");
const effortSelect = pick<HTMLSelectElement>("#effort");
const effortHint = pick("#effort-hint");
const apiKey = pick<HTMLInputElement>("#api-key");
const generateButton = pick<HTMLButtonElement>("#generate");
const generateMessage = pick("#generate-message");
const profileBody = pick("#profile-body");
const loadingStep = pick("#loading-step");

const views = ["setup", "inference", "generate", "loading", "profile", "dashboard"] as const;
type View = (typeof views)[number];

/** Where the inference form returns to: onboarding continues to generation, a later edit goes back. */
let afterInference: View = "generate";

let progressTimer: ReturnType<typeof setInterval> | undefined;

function showView(view: View) {
  for (const name of views) pick(`#view-${name}`).hidden = name !== view;
  clearInterval(progressTimer);
  if (view !== "loading") return;
  progressTimer = setInterval(async () => {
    const step = await invoke("get_progress").catch(() => "");
    if (step) loadingStep.textContent = step;
  }, 1500);
}

function showRuntime(next: RuntimeStatus) {
  runtime = next;
  runtimeMessage.textContent = next.message;
  statePill.textContent = next.state === "running" ? "Running" : next.state === "stopped" ? "Stopped" : "Unavailable";
  statePill.className = `pill ${next.state === "running" ? "ok" : next.state === "unavailable" ? "bad" : ""}`;
  readiness.innerHTML = `<span class="pill ${next.discordReady ? "ok" : "bad"}">Discord ${next.discordReady ? "ready" : "not ready"}</span><span class="pill ${next.memoryReady ? "ok" : "bad"}">Mnemosyne ${next.memoryReady ? "ready" : "not ready"}</span>`;
  startButton.disabled = next.state === "running";
  stopButton.disabled = next.state !== "running";
  resetButton.disabled = next.state === "unavailable";
}

function showConfig(next: WallisConfig) {
  applicationId.value = next.discordApplicationId;
  allowedUsers.value = next.discordAllowedUsers;
  autoStart.checked = next.autoStart;
  rootfsArchive.value = next.rootfsArchive;
  rootfsSha256.value = next.rootfsSha256;
  distroRoot.value = next.distroRoot;
  if (next.provider) providerSelect.value = next.provider;
  modelInput.value = next.model;
  if (effortSelect.querySelector(`option[value="${next.reasoningEffort}"]`)) effortSelect.value = next.reasoningEffort;
  inferenceSummary.textContent = next.model ? `Thinking with ${next.model} on ${next.provider} at ${next.reasoningEffort} reasoning effort.` : "";
}

function showModels(keepChoice = false, preferred?: string) {
  const models = providers.find((option) => option.id === providerSelect.value)?.models ?? [];
  modelOptions.innerHTML = models.map((model) => `<option value="${safe(model.id)}"></option>`).join("");
  if (!keepChoice || !modelInput.value) modelInput.value = models[0]?.id ?? "";
  showEfforts(preferred);
}

/** Only levels the chosen model can actually be asked for; a model we do not know gets the full ladder. */
function showEfforts(preferred = effortSelect.value) {
  const model = providers.find((option) => option.id === providerSelect.value)?.models.find((entry) => entry.id === modelInput.value.trim());
  const efforts = model?.efforts.length ? model.efforts : EFFORTS;
  const chosen = efforts.includes(preferred) ? preferred : (efforts.includes("medium") ? "medium" : efforts[efforts.length - 1]);
  effortSelect.innerHTML = efforts.map((effort) => `<option value="${safe(effort)}"${effort === chosen ? " selected" : ""}>${safe(EFFORT_LABELS[effort] ?? effort)}</option>`).join("");
  effortHint.textContent = efforts.length === 1 ? "This model does not think, so reasoning is off." : "";
}

/** Profile text is written by a model and the catalog comes off the network: never trust either as markup. */
const safe = (value: string) => value.replace(/[&<>"]/g, (character) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" })[character] ?? character);

function field(label: string, value: string) {
  return value ? `<div class="field"><dt>${label}</dt><dd>${safe(value)}</dd></div>` : "";
}

function showProfile(profile: Profile) {
  profileBody.innerHTML = `
    <p class="profile-name">${safe(profile.name)}${profile.age ? ` · ${safe(profile.age)}` : ""}${profile.pronouns ? ` · ${safe(profile.pronouns)}` : ""}</p>
    ${profile.tagline ? `<p class="tagline">“${safe(profile.tagline)}”</p>` : ""}
    ${profile.bio ? `<p>${safe(profile.bio)}</p>` : ""}
    ${profile.interests.length ? `<ul class="tags">${profile.interests.map((interest) => `<li>${safe(interest)}</li>`).join("")}</ul>` : ""}
    <dl>${field("Location", profile.location)}${field("Does", profile.occupation)}${field("Looking for", profile.lookingFor)}${field("Fun fact", profile.funFact)}</dl>`;
}

async function openProfile(next: View) {
  try {
    showProfile(await invoke("get_profile"));
    showView("profile");
  } catch (error) {
    generateMessage.textContent = error instanceof Error ? error.message : String(error);
    showView(next);
  }
}

async function load() {
  try {
    setup = await invoke("get_setup_status");
    setupMessage.textContent = setup.message;
    secret = await invoke("get_secret_status");
    tokenStatus.textContent = secret.hasToken ? "Discord token is stored securely." : "No Discord token is configured.";
    showRuntime(await invoke("get_runtime_status"));
    const stage = await invoke("get_stage");
    if (stage === "inference") await loadProviders();
    showConfig(await invoke("get_config"));
    showView(stage === "ready" ? "dashboard" : stage);
  } catch (error) {
    setupMessage.textContent = error instanceof Error ? error.message : "Tauri is not available.";
    showView("setup");
  }
}

async function loadProviders() {
  providers = await invoke("get_providers");
  providerSelect.innerHTML = providers.map((option) => `<option value="${safe(option.id)}">${safe(option.label)}</option>`).join("");
  showModels();
}

providerSelect.addEventListener("change", () => showModels());
modelInput.addEventListener("change", () => showEfforts());
modelInput.addEventListener("input", () => showEfforts());

/** A finished wizard step stays reachable: click it to go back and change that answer. */
async function goToStep(view: View, back: View) {
  if (view === "setup") {
    pick<HTMLButtonElement>("#setup-reset").hidden = false;
    installMessage.textContent = "Wallis is already imported. Reset it first if you need a different rootfs archive.";
  }
  if (view === "inference") {
    afterInference = back;
    const current = await invoke("get_config");
    await loadProviders();
    showConfig(current);
    showModels(true, current.reasoningEffort);
  }
  showView(view);
}

app.addEventListener("click", (event) => {
  const step = (event.target as HTMLElement).closest<HTMLElement>(".steps .done[data-view]");
  if (step) void goToStep(step.dataset.view as View, "generate");
});

pick("#change-model").addEventListener("click", () => void goToStep("inference", "dashboard"));

inferenceForm.addEventListener("submit", async (event) => {
  event.preventDefault();
  inferenceMessage.textContent = "Applying…";
  try {
    showConfig(await invoke("save_inference", { provider: providerSelect.value, model: modelInput.value, reasoningEffort: effortSelect.value, apiKey: apiKey.value }));
    apiKey.value = "";
    inferenceMessage.textContent = "";
    showView(afterInference);
  } catch (error) { inferenceMessage.textContent = error instanceof Error ? error.message : String(error); }
});

generateButton.addEventListener("click", async () => {
  showView("loading");
  try {
    showProfile(await invoke("generate_wallis"));
    showView("profile");
  } catch (error) {
    generateMessage.textContent = error instanceof Error ? error.message : String(error);
    showView("generate");
  }
});

pick("#profile-done").addEventListener("click", async () => { showView("dashboard"); showRuntime(await invoke("get_runtime_status")); });
pick("#profile-refresh").addEventListener("click", () => void openProfile("profile"));
pick("#open-profile").addEventListener("click", () => void openProfile("dashboard"));

startButton.addEventListener("click", async () => {
  try { showRuntime(await invoke("start_wallis")); } catch (error) { showRuntime({ state: "unavailable", message: String(error), discordReady: false, memoryReady: false }); }
});
stopButton.addEventListener("click", async () => {
  try { showRuntime(await invoke("stop_wallis")); } catch (error) { showRuntime({ state: "unavailable", message: String(error), discordReady: false, memoryReady: false }); }
});
async function reset() {
  if (!window.confirm("Reset Wallis? This permanently deletes the distro, its memory, and the profile Wallis wrote for himself.")) return;
  try { showRuntime(await invoke("reset_wallis")); } catch (error) { showRuntime({ state: "unavailable", message: String(error), discordReady: false, memoryReady: false }); }
  installMessage.textContent = "";
  await load();
}

resetButton.addEventListener("click", () => void reset());
pick("#setup-reset").addEventListener("click", () => void reset());
installForm.addEventListener("submit", async (event) => {
  event.preventDefault();
  installMessage.textContent = "Verifying and importing…";
  try {
    const next = await invoke("import_wallis", { rootfsArchive: rootfsArchive.value.trim(), expectedSha256: rootfsSha256.value.trim(), installRoot: distroRoot.value.trim() || null });
    installMessage.textContent = next.message;
    await load();
  } catch (error) { installMessage.textContent = error instanceof Error ? error.message : String(error); }
});
form.addEventListener("submit", async (event) => {
  event.preventDefault();
  const current = await invoke("get_config");
  const next: WallisConfig = { ...current, discordApplicationId: applicationId.value.trim(), discordAllowedUsers: allowedUsers.value.trim(), autoStart: autoStart.checked, rootfsArchive: rootfsArchive.value.trim(), rootfsSha256: rootfsSha256.value.trim(), distroRoot: distroRoot.value.trim() };
  const error = validateConfig(next);
  if (error) { configMessage.textContent = error; return; }
  try {
    showConfig(await invoke("save_config", { config: next }));
    if (token.value) secret = await invoke("save_discord_token", { token: token.value });
    token.value = "";
    tokenStatus.textContent = secret.hasToken ? "Discord token is stored securely." : "No Discord token is configured.";
    configMessage.textContent = "Configuration saved.";
  }
  catch (saveError) { configMessage.textContent = saveError instanceof Error ? saveError.message : "Unable to save configuration."; }
});

function appendTerminal(command: string, output: string) {
  const line = document.createElement("div");
  const prompt = document.createElement("b");
  prompt.textContent = `$ ${command}`;
  line.append(prompt, document.createTextNode(`\n${output.replace(/\s+$/, "")}\n\n`));
  terminalOutput.append(line);
  terminalOutput.scrollTop = terminalOutput.scrollHeight;
}

terminalForm.addEventListener("submit", async (event) => {
  event.preventDefault();
  const command = terminalCommand.value.trim();
  if (!command) return;
  terminalCommand.value = "";
  try { appendTerminal(command, await invoke("run_in_distro", { command })); }
  catch (error) { appendTerminal(command, error instanceof Error ? error.message : String(error)); }
});
openTerminalButton.addEventListener("click", async () => {
  try { await invoke("open_terminal"); } catch (error) { appendTerminal("open shell window", error instanceof Error ? error.message : String(error)); }
});

void load();
