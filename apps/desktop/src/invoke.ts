export type SetupStatus = {
  wsl2Available: boolean;
  wslVersion: string;
  message: string;
};

export type RuntimeStatus = {
  state: "stopped" | "running" | "unavailable";
  message: string;
  discordReady: boolean;
  memoryReady: boolean;
};

export type SecretStatus = { hasToken: boolean };

export type WallisConfig = {
  discordApplicationId: string;
  discordAllowedUsers: string;
  personaName: string;
  memoryProvider: "mnemosyne";
  autoStart: boolean;
  rootfsArchive: string;
  rootfsSha256: string;
  distroRoot: string;
};

type Commands = {
  get_setup_status: SetupStatus;
  get_runtime_status: RuntimeStatus;
  start_hermes: RuntimeStatus;
  stop_hermes: RuntimeStatus;
  reset_wallis: RuntimeStatus;
  import_wallis: RuntimeStatus;
  get_config: WallisConfig;
  save_config: WallisConfig;
  get_secret_status: SecretStatus;
  save_discord_token: SecretStatus;
};

declare global {
  interface Window {
    __TAURI__?: { core: { invoke<T>(command: string, args?: unknown): Promise<T> } };
  }
}

const fallback: WallisConfig = {
  discordApplicationId: "",
  discordAllowedUsers: "",
  personaName: "Wallis",
  memoryProvider: "mnemosyne",
  autoStart: false,
  rootfsArchive: "",
  rootfsSha256: "",
  distroRoot: "",
};

export function invoke<K extends keyof Commands>(command: K, args?: unknown): Promise<Commands[K]> {
  return window.__TAURI__?.core.invoke<Commands[K]>(command, args) ?? Promise.reject(new Error("Wallis must run inside Tauri"));
}

export function defaultConfig(): WallisConfig {
  return { ...fallback };
}
