import "./styles.css";
import { validateConfig } from "./config";
import { invoke, type RuntimeStatus, type SecretStatus, type SetupStatus, type WallisConfig } from "./invoke";

const app = document.querySelector<HTMLDivElement>("#app");
if (!app) throw new Error("Wallis mount is missing");

let setup: SetupStatus = { wsl2Available: false, wslVersion: "unknown", message: "Checking WSL2…" };
let runtime: RuntimeStatus = { state: "unavailable", message: "Checking runtime…", discordReady: false, memoryReady: false };
let secret: SecretStatus = { hasToken: false };

app.innerHTML = `
  <header><div><p class="eyebrow">LOCAL-FIRST DISCORD BOT</p><h1>Wallis</h1><p>One calm place to set up, inspect, and control your local agent.</p></div><p id="state-pill" class="pill">Checking…</p></header>
  <section aria-labelledby="setup-title"><h2 id="setup-title">Setup</h2><p id="setup-message" role="status">${setup.message}</p><form id="install-form">
    <label for="rootfs-archive">Wallis rootfs archive</label><input id="rootfs-archive" name="rootfsArchive" placeholder="C:\\Downloads\\wallis-rootfs.tar" autocomplete="off" required />
    <label for="rootfs-sha256">Expected SHA-256</label><input id="rootfs-sha256" name="rootfsSha256" placeholder="64 hexadecimal characters" autocomplete="off" required />
    <label for="distro-root">Distro install root <span>(optional)</span></label><input id="distro-root" name="distroRoot" placeholder="Default: %LOCALAPPDATA%\\Wallis\\distro" autocomplete="off" />
    <button type="submit">Import verified Wallis distro</button><p id="install-message" role="status" aria-live="polite"></p>
  </form></section>
  <section aria-labelledby="runtime-title"><h2 id="runtime-title">Runtime</h2><p id="runtime-message" role="status">${runtime.message}</p><div id="readiness" class="pills"></div><div class="actions"><button id="start" type="button">Start Hermes</button><button id="stop" type="button" class="secondary">Stop Hermes</button><button id="reset" type="button" class="secondary">Reset Wallis</button></div></section>
  <section aria-labelledby="terminal-title"><h2 id="terminal-title">Terminal</h2><p class="hint">Runs one command at a time inside the Wallis distro as the non-root <code>wallis</code> user.</p>
    <pre id="terminal-output" role="log" aria-live="polite" tabindex="0">Ready.</pre>
    <form id="terminal-form" class="prompt"><span aria-hidden="true">$</span><input id="terminal-command" name="command" placeholder="hermes --help" autocomplete="off" spellcheck="false" required aria-label="Command to run in the Wallis distro" /><button type="submit">Run</button><button id="open-terminal" type="button" class="secondary">Open shell window</button></form></section>
  <section aria-labelledby="config-title"><h2 id="config-title">Configuration</h2><form id="config-form">
    <label for="application-id">Discord application ID <span>(optional until bot setup)</span></label><input id="application-id" name="discordApplicationId" inputmode="numeric" autocomplete="off" />
    <label for="allowed-users">Allowed Discord user IDs</label><input id="allowed-users" name="discordAllowedUsers" inputmode="numeric" placeholder="123456789012345678,198765432109876543" autocomplete="off" required /><p class="hint">Only these users can interact with Wallis. Use numeric Discord user IDs separated by commas.</p>
    <label for="discord-token">Discord bot token</label><input id="discord-token" name="discordToken" type="password" autocomplete="new-password" placeholder="Leave blank to keep the saved token" /><p id="token-status" class="hint" role="status"></p>
    <label for="persona-name">Persona name</label><input id="persona-name" name="personaName" required />
    <label class="check"><input id="auto-start" name="autoStart" type="checkbox" /> Start Hermes when Wallis opens</label>
    <p>Memory provider: <strong>Mnemosyne</strong></p><p class="hint">Bot tokens are protected with Windows DPAPI and never returned to the UI.</p>
    <button type="submit">Save configuration</button><p id="config-message" role="status" aria-live="polite"></p>
  </form></section>`;

const setupMessage = document.querySelector<HTMLElement>("#setup-message")!;
const installMessage = document.querySelector<HTMLElement>("#install-message")!;
const runtimeMessage = document.querySelector<HTMLElement>("#runtime-message")!;
const readiness = document.querySelector<HTMLElement>("#readiness")!;
const statePill = document.querySelector<HTMLElement>("#state-pill")!;
const terminalOutput = document.querySelector<HTMLElement>("#terminal-output")!;
const terminalForm = document.querySelector<HTMLFormElement>("#terminal-form")!;
const terminalCommand = document.querySelector<HTMLInputElement>("#terminal-command")!;
const openTerminalButton = document.querySelector<HTMLButtonElement>("#open-terminal")!;
const configMessage = document.querySelector<HTMLElement>("#config-message")!;
const tokenStatus = document.querySelector<HTMLElement>("#token-status")!;
const startButton = document.querySelector<HTMLButtonElement>("#start")!;
const stopButton = document.querySelector<HTMLButtonElement>("#stop")!;
const resetButton = document.querySelector<HTMLButtonElement>("#reset")!;
const installForm = document.querySelector<HTMLFormElement>("#install-form")!;
const form = document.querySelector<HTMLFormElement>("#config-form")!;
const applicationId = document.querySelector<HTMLInputElement>("#application-id")!;
const allowedUsers = document.querySelector<HTMLInputElement>("#allowed-users")!;
const token = document.querySelector<HTMLInputElement>("#discord-token")!;
const personaName = document.querySelector<HTMLInputElement>("#persona-name")!;
const autoStart = document.querySelector<HTMLInputElement>("#auto-start")!;
const rootfsArchive = document.querySelector<HTMLInputElement>("#rootfs-archive")!;
const rootfsSha256 = document.querySelector<HTMLInputElement>("#rootfs-sha256")!;
const distroRoot = document.querySelector<HTMLInputElement>("#distro-root")!;

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
  personaName.value = next.personaName;
  autoStart.checked = next.autoStart;
  rootfsArchive.value = next.rootfsArchive;
  rootfsSha256.value = next.rootfsSha256;
  distroRoot.value = next.distroRoot;
}

async function load() {
  try {
    setup = await invoke("get_setup_status");
    setupMessage.textContent = setup.message;
    secret = await invoke("get_secret_status");
    tokenStatus.textContent = secret.hasToken ? "Discord token is stored securely." : "No Discord token is configured.";
    showRuntime(await invoke("get_runtime_status"));
    showConfig(await invoke("get_config"));
  } catch (error) {
    setupMessage.textContent = error instanceof Error ? error.message : "Tauri is not available.";
  }
}

startButton.addEventListener("click", async () => {
  try { showRuntime(await invoke("start_hermes")); } catch (error) { showRuntime({ state: "unavailable", message: String(error), discordReady: false, memoryReady: false }); }
});
stopButton.addEventListener("click", async () => {
  try { showRuntime(await invoke("stop_hermes")); } catch (error) { showRuntime({ state: "unavailable", message: String(error), discordReady: false, memoryReady: false }); }
});
resetButton.addEventListener("click", async () => {
  if (!window.confirm("Reset Wallis? This permanently deletes the distro and its memory.")) return;
  try { showRuntime(await invoke("reset_wallis")); } catch (error) { showRuntime({ state: "unavailable", message: String(error), discordReady: false, memoryReady: false }); }
});
installForm.addEventListener("submit", async (event) => {
  event.preventDefault();
  installMessage.textContent = "Verifying and importing…";
  try {
    const next = await invoke("import_wallis", { rootfsArchive: rootfsArchive.value.trim(), expectedSha256: rootfsSha256.value.trim(), installRoot: distroRoot.value.trim() || null });
    installMessage.textContent = next.message;
    showRuntime(await invoke("get_runtime_status"));
  } catch (error) { installMessage.textContent = error instanceof Error ? error.message : String(error); }
});
form.addEventListener("submit", async (event) => {
  event.preventDefault();
  const next: WallisConfig = { discordApplicationId: applicationId.value.trim(), discordAllowedUsers: allowedUsers.value.trim(), personaName: personaName.value.trim(), memoryProvider: "mnemosyne", autoStart: autoStart.checked, rootfsArchive: rootfsArchive.value.trim(), rootfsSha256: rootfsSha256.value.trim(), distroRoot: distroRoot.value.trim() };
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
