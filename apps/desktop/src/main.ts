import "./styles.css";
import { validateConfig } from "./config";
import { Chart } from "chart.js/auto";
import { EFFORTS, EFFORT_LABELS, invoke, type VoiceCycle, type VoiceOption, type Profile, type ProviderOption, type RuntimeStatus, type SecretStatus, type SetupStatus, type WallisConfig } from "./invoke";

const app = document.querySelector<HTMLDivElement>("#app");
if (!app) throw new Error("Wallis mount is missing");

let setup: SetupStatus = { wsl2Available: false, wslVersion: "unknown", message: "Checking WSL2…" };
let secret: SecretStatus = { hasToken: false };
let providers: ProviderOption[] = [];

app.innerHTML = `
  <header><h1>Wallis</h1><p id="state-pill" class="pill">Checking…</p></header>

  <div id="view-boot">
    <section class="centered loading" aria-live="polite"><h2>Starting Wallis</h2>
      <progress id="boot-progress" max="6" value="0"></progress>
      <p class="hint" id="boot-step">Checking WSL2…</p>
    </section>
  </div>

  <div id="view-setup" hidden>
    <section aria-labelledby="setup-title"><h2 id="setup-title">Setup</h2><p id="setup-message" role="status">${setup.message}</p><form id="install-form">
      <label for="rootfs-archive">Wallis rootfs archive</label><input id="rootfs-archive" name="rootfsArchive" placeholder="C:\\Downloads\\wallis-rootfs.tar" autocomplete="off" required />
      <label for="rootfs-sha256">Expected SHA-256</label><input id="rootfs-sha256" name="rootfsSha256" placeholder="64 hexadecimal characters" autocomplete="off" required />
      <label for="distro-root">Distro install root <span>(optional)</span></label><input id="distro-root" name="distroRoot" placeholder="Default: %LOCALAPPDATA%\\Wallis\\distro" autocomplete="off" />
      <button type="submit">Import distro</button><button id="setup-reset" type="button" class="secondary" hidden>Reset Wallis</button><p id="install-message" role="status" aria-live="polite"></p>
    </form></section>
  </div>

  <div id="view-inference" hidden>
    <div id="inference-host"></div>
  </div>

  <div id="view-generate" hidden>
    <section class="centered" aria-labelledby="generate-title"><h2 id="generate-title">Generate Wallis</h2>
      <p>Wallis writes his own profile, then downloads his voice models and Discord bot — about 400 MB into the distro.</p>
      <div class="actions"><button id="generate" type="button">Generate Wallis</button></div>
      <p id="generate-message" role="status" aria-live="polite"></p>
    </section>
  </div>

  <div id="view-loading" hidden>
    <section class="centered loading" aria-live="polite"><div class="spinner" aria-hidden="true"></div>
      <h2>Generating Wallis</h2><p class="hint" id="loading-step">Setting up memory, voice, and the Discord bot.</p>
    </section>
  </div>

  <div id="view-profile" hidden>
    <section id="profile-card" aria-labelledby="profile-title"><h2 id="profile-title">Profile</h2><div id="profile-body"></div>
      <div class="actions"><button id="profile-done" type="button">Continue</button><button id="profile-refresh" type="button" class="secondary">Refresh</button></div>
    </section>
  </div>

  <div id="view-dashboard" hidden>
    <nav class="tabs" role="tablist">
      <button id="tab-home" type="button" role="tab" aria-selected="true">Home</button>
      <button id="tab-profile" type="button" role="tab" aria-selected="false">Profile</button>
      <button id="tab-settings" type="button" role="tab" aria-selected="false">Settings</button>
    </nav>

    <div id="pane-home" role="tabpanel" aria-labelledby="tab-home">
      <section aria-labelledby="runtime-title"><h2 id="runtime-title">Runtime</h2><p id="runtime-message" role="status"></p><div id="readiness" class="pills"></div><div class="actions"><button id="start" type="button">Start Wallis</button><button id="stop" type="button" class="secondary">Stop Wallis</button><button id="reset" type="button" class="secondary">Reset Wallis</button></div></section>
      <section aria-labelledby="performance-title"><h2 id="performance-title">Conversational Performance</h2>
        <p class="hint">Delay per stage of the last 25 round trips, in milliseconds.</p>
        <div class="chart"><canvas id="performance-chart" role="img" aria-label="Stacked bar chart of speech recognition, response and speech delays per round trip"></canvas></div>
        <p id="performance-message" class="hint" role="status"></p></section>
      <section aria-labelledby="terminal-title"><h2 id="terminal-title">Terminal</h2>
        <pre id="terminal-output" role="log" aria-live="polite" tabindex="0">Ready.</pre>
        <form id="terminal-form" class="prompt"><span aria-hidden="true">$</span><input id="terminal-command" name="command" placeholder="hermes --help" autocomplete="off" spellcheck="false" required aria-label="Command to run in the Wallis distro" /><button type="submit">Run</button><button id="open-terminal" type="button" class="secondary">Open shell window</button></form></section>
    </div>

    <div id="pane-profile" role="tabpanel" aria-labelledby="tab-profile" hidden><div id="profile-host"></div></div>

    <div id="pane-settings" role="tabpanel" aria-labelledby="tab-settings" hidden>
      <div id="inference-home"><section aria-labelledby="inference-title"><h2 id="inference-title">AI model</h2><form id="inference-form">
        <label for="provider">Provider</label><div class="input-action"><select id="provider" name="provider"></select><button id="refresh-providers" type="button" class="secondary">Refresh</button></div>
        <label for="model">Model</label><input id="model" name="model" list="model-options" autocomplete="off" placeholder="anthropic/claude-opus-5" required /><datalist id="model-options"></datalist>
        <label for="effort">Reasoning effort</label><select id="effort" name="reasoningEffort"></select><p class="hint" id="effort-hint"></p>
        <label for="api-key">Provider API key</label><input id="api-key" name="apiKey" type="password" autocomplete="new-password" placeholder="Leave blank to keep the saved key" />
        <button type="submit">Save model</button><p id="inference-message" role="status" aria-live="polite"></p>
      </form></section></div>
      <section aria-labelledby="voice-title"><h2 id="voice-title">Voice</h2><form id="voice-form">
        <label for="stt-model">Speech recognition model</label><select id="stt-model" name="sttModel"></select>
        <label for="stt-threads">Speech recognition threads</label><input id="stt-threads" name="sttThreads" type="number" min="1" max="64" step="1" />
        <label for="tts-voice">Voice quality</label><select id="tts-voice" name="ttsVoice"></select>
        <label for="tts-speed">Speaking speed</label><input id="tts-speed" name="ttsSpeed" type="number" min="0.5" max="2" step="0.05" />
        <label for="silence-ms">Silence before Wallis answers (ms)</label><input id="silence-ms" name="silenceMs" type="number" min="100" max="2000" step="50" />
        <button type="submit">Save voice</button><p id="voice-message" role="status" aria-live="polite"></p>
      </form></section>
      <section aria-labelledby="config-title"><h2 id="config-title">Discord</h2><form id="config-form">
        <label for="application-id">Application ID</label><input id="application-id" name="discordApplicationId" inputmode="numeric" autocomplete="off" />
        <label for="allowed-users">Allowed user IDs</label><input id="allowed-users" name="discordAllowedUsers" inputmode="numeric" placeholder="123456789012345678,198765432109876543" autocomplete="off" required />
        <label for="discord-token">Bot token</label><input id="discord-token" name="discordToken" type="password" autocomplete="new-password" placeholder="Leave blank to keep the saved token" /><p id="token-status" class="hint" role="status"></p>
        <label class="check"><input id="auto-start" name="autoStart" type="checkbox" /> Start Wallis when the app opens</label>
        <button type="submit">Save Discord settings</button><p id="config-message" role="status" aria-live="polite"></p>
      </form></section>
    </div>
  </div>`;

const pick = <T extends HTMLElement>(id: string) => document.querySelector<T>(id)!;
const setupMessage = pick("#setup-message");
const installMessage = pick("#install-message");
const runtimeMessage = pick("#runtime-message");
const readiness = pick("#readiness");
const statePill = pick("#state-pill");
const configMessage = pick("#config-message");
const tokenStatus = pick("#token-status");
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
const inferenceSection = inferenceForm.closest("section")!;
const inferenceSubmit = pick<HTMLButtonElement>('#inference-form button[type="submit"]');
const inferenceMessage = pick("#inference-message");
const providerSelect = pick<HTMLSelectElement>("#provider");
const refreshProvidersButton = pick<HTMLButtonElement>("#refresh-providers");
const modelInput = pick<HTMLInputElement>("#model");
const modelOptions = pick("#model-options");
const effortSelect = pick<HTMLSelectElement>("#effort");
const effortHint = pick("#effort-hint");
const apiKey = pick<HTMLInputElement>("#api-key");
const generateButton = pick<HTMLButtonElement>("#generate");
const generateMessage = pick("#generate-message");
const profileBody = pick("#profile-body");
const loadingStep = pick("#loading-step");
const voiceForm = pick<HTMLFormElement>("#voice-form");
const voiceMessage = pick("#voice-message");
const sttModel = pick<HTMLSelectElement>("#stt-model");
const sttThreads = pick<HTMLInputElement>("#stt-threads");
const ttsVoice = pick<HTMLSelectElement>("#tts-voice");
const ttsSpeed = pick<HTMLInputElement>("#tts-speed");
const silenceMs = pick<HTMLInputElement>("#silence-ms");
const profileCard = pick("#profile-card");
const profileDone = pick<HTMLButtonElement>("#profile-done");
const performanceMessage = pick("#performance-message");
const bootProgress = pick<HTMLProgressElement>("#boot-progress");
const bootStep = pick("#boot-step");

const views = ["boot", "setup", "inference", "generate", "loading", "profile", "dashboard"] as const;
type View = (typeof views)[number];

let progressTimer: ReturnType<typeof setInterval> | undefined;

const tabs = ["home", "profile", "settings"] as const;
type Tab = (typeof tabs)[number];

const WIZARD = { setup: "Import distro", inference: "Choose a model", generate: "Generate Wallis" } as const;
type Step = keyof typeof WIZARD;
const steps = Object.keys(WIZARD) as Step[];
const stepBar = document.createElement("ol");
stepBar.className = "steps";
/** How far the wizard has been taken; every step up to here stays reachable in both directions. */
let reached = 0;

/** One step bar, moved into whichever wizard view is showing, so the steps never drift apart. */
function showSteps(view: View) {
  const at = steps.indexOf(view as Step);
  if (at < 0) return;
  reached = Math.max(reached, at);
  pick(`#view-${view}`).prepend(stepBar);
  stepBar.innerHTML = steps.map((step, index) =>
    `<li class="${index === at ? "now" : index <= reached ? "done" : ""}"${index !== at && index <= reached ? ` data-view="${step}"` : ""}>${WIZARD[step]}</li>`).join("");
}

/** The model form and the profile card exist once and move between the wizard and the tabs. */
function showView(view: View) {
  showSteps(view);
  pick(view === "inference" ? "#inference-host" : "#inference-home").append(inferenceSection);
  inferenceSubmit.textContent = view === "inference" ? "Continue" : "Save model";
  if (view === "profile") { pick("#view-profile").append(profileCard); profileDone.hidden = false; }
  for (const name of views) pick(`#view-${name}`).hidden = name !== view;
  if (view === "dashboard") showTab("home"); else watchCycles(false);
  clearInterval(progressTimer);
  if (view !== "loading") return;
  progressTimer = setInterval(async () => {
    const step = await invoke("get_progress").catch(() => "");
    if (step) loadingStep.textContent = step;
  }, 1500);
}

function showTab(tab: Tab) {
  for (const name of tabs) {
    pick(`#pane-${name}`).hidden = name !== tab;
    pick(`#tab-${name}`).setAttribute("aria-selected", String(name === tab));
  }
  watchCycles(tab === "home");
  if (tab !== "profile") return;
  pick("#profile-host").append(profileCard);
  profileDone.hidden = true;
  void loadProfile();
}

const STAGES = [
  { key: "sttMs", label: "Speech recognition", color: "#6f7d90" },
  { key: "llmMs", label: "Response", color: "#92d3b4" },
  { key: "ttsMs", label: "Speech", color: "#7fa8d9" },
] as const;

const performanceChart = new Chart(pick<HTMLCanvasElement>("#performance-chart"), {
  type: "bar",
  data: { labels: [] as string[], datasets: STAGES.map((stage) => ({ label: stage.label, data: [] as number[], backgroundColor: stage.color })) },
  options: {
    animation: false,
    maintainAspectRatio: false,
    scales: {
      x: { stacked: true, grid: { display: false }, ticks: { color: "#6f7d90" } },
      y: { stacked: true, beginAtZero: true, grid: { color: "#262e3a" }, ticks: { color: "#6f7d90" }, title: { display: true, text: "ms", color: "#93a0b2" } },
    },
    plugins: { legend: { labels: { color: "#93a0b2" } }, tooltip: { callbacks: { label: (item) => `${item.dataset.label}: ${item.formattedValue} ms` } } },
  },
});

function showCycles(cycles: VoiceCycle[]) {
  performanceChart.data.labels = cycles.map((cycle) => new Date(cycle.at).toLocaleTimeString());
  for (const [index, stage] of STAGES.entries()) performanceChart.data.datasets[index]!.data = cycles.map((cycle) => cycle[stage.key]);
  performanceChart.update();
  performanceMessage.textContent = cycles.length ? "" : "No round trips yet. Talk to Wallis in a voice channel.";
}

/** The chart only matters while the home tab is open, so it polls there and nowhere else. */
let cyclesTimer: ReturnType<typeof setInterval> | undefined;
const loadCycles = async () => showCycles((await invoke("get_voice_cycles").catch(() => [])) ?? []);
function watchCycles(active: boolean) {
  clearInterval(cyclesTimer);
  if (!active) return;
  void loadCycles();
  cyclesTimer = setInterval(() => void loadCycles(), 5000);
}

function showRuntime(next: RuntimeStatus) {
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
  sttModel.value = next.sttModel;
  sttThreads.value = String(next.sttThreads);
  ttsVoice.value = next.ttsVoice;
  ttsSpeed.value = String(next.ttsSpeed);
  silenceMs.value = String(next.silenceMs);
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

async function loadProfile() {
  try { showProfile(await invoke("get_profile")); }
  catch (error) { profileBody.textContent = error instanceof Error ? error.message : String(error); }
}

function voiceOptions(select: HTMLSelectElement, options: VoiceOption[]) {
  select.innerHTML = options.map((option) => `<option value="${safe(option.id)}">${safe(option.label)} · ${option.megabytes} MB</option>`).join("");
}

voiceForm.addEventListener("submit", async (event) => {
  event.preventDefault();
  const current = await invoke("get_config");
  const next: WallisConfig = { ...current, sttModel: sttModel.value, sttThreads: Number(sttThreads.value), ttsVoice: ttsVoice.value, ttsSpeed: Number(ttsSpeed.value), silenceMs: Number(silenceMs.value) };
  try {
    showConfig(await invoke("save_config", { config: next }));
    voiceMessage.textContent = "Saved. Restart Wallis to pick up the change; a new model downloads on the next start.";
  } catch (error) { voiceMessage.textContent = error instanceof Error ? error.message : String(error); }
});

async function step<T>(label: string, work: Promise<T>): Promise<T> {
  bootStep.textContent = label;
  const result = await work;
  bootProgress.value += 1;
  return result;
}

async function load() {
  showView("boot");
  bootProgress.value = 0;
  reached = 0;
  try {
    setup = await step("Checking WSL2…", invoke("get_setup_status"));
    setupMessage.textContent = setup.message;
    secret = await step("Checking stored secrets…", invoke("get_secret_status"));
    tokenStatus.textContent = secret.hasToken ? "Discord token is stored securely." : "No Discord token is configured.";
    showRuntime(await step("Checking the runtime…", invoke("get_runtime_status")));
    const [sttOptions, ttsOptions] = await step("Loading voice options…", invoke("get_voice_options"));
    voiceOptions(sttModel, sttOptions);
    voiceOptions(ttsVoice, ttsOptions);
    const stage = await step("Working out where you left off…", invoke("get_stage"));
    if (stage !== "setup") await step("Asking Hermes for providers and models…", loadProviders().catch(() => {}));
    showConfig(await step("Loading your configuration…", invoke("get_config")));
    showModels(true);
    showView(stage === "ready" ? "dashboard" : (stage as View));
  } catch (error) {
    setupMessage.textContent = error instanceof Error ? error.message : "Tauri is not available.";
    showView("setup");
  }
}

async function loadProviders(refresh = false) {
  const [selectedProvider, selectedModel] = [providerSelect.value, modelInput.value];
  providers = await invoke("get_providers", { refresh });
  providerSelect.innerHTML = providers.map((option) => `<option value="${safe(option.id)}">${safe(option.label)}</option>`).join("");
  if (providers.some((option) => option.id === selectedProvider)) providerSelect.value = selectedProvider;
  showModels(true);
  if (selectedModel) { modelInput.value = selectedModel; showEfforts(); }
}

refreshProvidersButton.addEventListener("click", async () => {
  refreshProvidersButton.disabled = true;
  inferenceMessage.textContent = "Refreshing providers and models from Hermes…";
  try {
    await loadProviders(true);
    inferenceMessage.textContent = `Synced ${providers.length} providers from Hermes.`;
  } catch (error) { inferenceMessage.textContent = error instanceof Error ? error.message : String(error); }
  finally { refreshProvidersButton.disabled = false; }
});

providerSelect.addEventListener("change", () => showModels());
modelInput.addEventListener("change", () => showEfforts());
modelInput.addEventListener("input", () => showEfforts());

/** A finished wizard step stays reachable: click it to go back and change that answer. */
async function goToStep(view: View) {
  if (view === "setup") {
    pick<HTMLButtonElement>("#setup-reset").hidden = false;
    installMessage.textContent = "Wallis is already imported. Reset it first if you need a different rootfs archive.";
  }
  if (view === "inference" && !providers.length) await loadProviders().catch(() => {});
  showView(view);
}

app.addEventListener("click", (event) => {
  const crumb = (event.target as HTMLElement).closest<HTMLElement>(".steps .done[data-view]");
  if (crumb) void goToStep(crumb.dataset.view as View);
});

inferenceForm.addEventListener("submit", async (event) => {
  event.preventDefault();
  inferenceMessage.textContent = "Applying…";
  try {
    await invoke("save_inference", { provider: providerSelect.value, model: modelInput.value, reasoningEffort: effortSelect.value, apiKey: apiKey.value });
    apiKey.value = "";
    inferenceMessage.textContent = "Saved.";
    if (!pick("#view-inference").hidden) showView("generate");
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

profileDone.addEventListener("click", async () => { showView("dashboard"); showRuntime(await invoke("get_runtime_status")); });
pick("#profile-refresh").addEventListener("click", () => void loadProfile());
for (const tab of tabs) pick(`#tab-${tab}`).addEventListener("click", () => showTab(tab));

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
