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

/** Which screen the app owes the user, decided in Rust from the distro and config. */
export type Stage = "setup" | "inference" | "generate" | "ready";

export type ModelOption = { id: string; efforts: string[] };

export type ProviderOption = { id: string; label: string; models: ModelOption[] };

export type VoiceOption = { id: string; label: string; megabytes: number };

export type Profile = {
  name: string;
  age: string;
  pronouns: string;
  location: string;
  occupation: string;
  tagline: string;
  bio: string;
  interests: string[];
  lookingFor: string;
  funFact: string;
};

/** One Wallis round trip: the delay of each pipeline stage, in milliseconds. */
export type VoiceCycle = { at: string; sttMs: number; llmMs: number; ttsMs: number };

export type WallisConfig = {
  discordApplicationId: string;
  discordAllowedUsers: string;
  discordReadChannels: string;
  discordWriteChannels: string;
  discordVoiceTextChannel: string;
  voiceParticipation: "mentions" | "natural";
  textParticipation: "mentions" | "natural";
  participationCooldownSecs: number;
  memoryProvider: "mnemosyne";
  autoStart: boolean;
  rootfsArchive: string;
  rootfsSha256: string;
  distroRoot: string;
  provider: string;
  model: string;
  backgroundProvider: string;
  backgroundModel: string;
  backgroundReasoningEffort: string;
  reasoningEffort: string;
  onboarded: boolean;
  sttModel: string;
  sttThreads: number;
  ttsVoice: string;
  ttsSpeed: number;
  silenceMs: number;
};

type Commands = {
  get_setup_status: SetupStatus;
  get_runtime_status: RuntimeStatus;
  get_stage: Stage;
  get_progress: string;
  get_voice_options: [VoiceOption[], VoiceOption[]];
  get_voice_cycles: VoiceCycle[];
  start_wallis: RuntimeStatus;
  stop_wallis: RuntimeStatus;
  reset_wallis: RuntimeStatus;
  import_wallis: RuntimeStatus;
  get_config: WallisConfig;
  save_config: WallisConfig;
  get_secret_status: SecretStatus;
  save_discord_token: SecretStatus;
  run_in_distro: string;
  open_terminal: null;
  get_providers: ProviderOption[];
  get_profile: Profile;
  save_inference: WallisConfig;
  save_background_inference: WallisConfig;
  generate_wallis: Profile;
};

declare global {
  interface Window {
    __TAURI__?: { core: { invoke<T>(command: string, args?: unknown): Promise<T> } };
  }
}

/** Full ladder, used when a model's supported levels are unknown. `none` turns thinking off. */
export const EFFORTS = ["none", "minimal", "low", "medium", "high", "xhigh", "max"];

export const EFFORT_LABELS: Record<string, string> = { none: "Off — no thinking", minimal: "Minimal", low: "Low", medium: "Medium", high: "High", xhigh: "Extra high", max: "Max" };

const fallback: WallisConfig = {
  discordApplicationId: "",
  discordAllowedUsers: "",
  discordReadChannels: "",
  discordWriteChannels: "",
  discordVoiceTextChannel: "",
  voiceParticipation: "natural",
  textParticipation: "natural",
  participationCooldownSecs: 5,
  memoryProvider: "mnemosyne",
  autoStart: false,
  rootfsArchive: "",
  rootfsSha256: "",
  distroRoot: "",
  provider: "",
  model: "",
  backgroundProvider: "",
  backgroundModel: "",
  backgroundReasoningEffort: "high",
  reasoningEffort: "medium",
  onboarded: false,
  sttModel: "base.en",
  sttThreads: 4,
  ttsVoice: "medium",
  ttsSpeed: 1,
  silenceMs: 350,
};

export function invoke<K extends keyof Commands>(command: K, args?: unknown): Promise<Commands[K]> {
  return window.__TAURI__?.core.invoke<Commands[K]>(command, args) ?? Promise.reject(new Error("Wallis must run inside Tauri"));
}

export function defaultConfig(): WallisConfig {
  return { ...fallback };
}
