export type SetupStatus = {
  wsl2Available: boolean;
  message: string;
};

export type RuntimeStatus = {
  state: "stopped" | "running" | "unavailable";
  message: string;
};

export type WallisConfig = {
  discordApplicationId: string;
  personaName: string;
  memoryProvider: "mnemosyne";
  autoStart: boolean;
};

type Commands = {
  get_setup_status: SetupStatus;
  get_runtime_status: RuntimeStatus;
  start_hermes: RuntimeStatus;
  stop_hermes: RuntimeStatus;
  get_config: WallisConfig;
  save_config: WallisConfig;
};

declare global {
  interface Window {
    __TAURI__?: { core: { invoke<T>(command: string, args?: unknown): Promise<T> } };
  }
}

const fallback: WallisConfig = {
  discordApplicationId: "",
  personaName: "Wallis",
  memoryProvider: "mnemosyne",
  autoStart: false,
};

export function invoke<K extends keyof Commands>(command: K, args?: unknown): Promise<Commands[K]> {
  return window.__TAURI__?.core.invoke<Commands[K]>(command, args) ?? Promise.reject(new Error("Wallis must run inside Tauri"));
}

export function defaultConfig(): WallisConfig {
  return { ...fallback };
}
