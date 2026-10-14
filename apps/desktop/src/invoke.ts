export type RuntimeStatus = { state: "stopped" | "running" | "unavailable"; message: string };

export type SecretStatus = { hasToken: boolean };

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

export type WallisConfig = {
  discordApplicationId: string;
  discordAllowedUsers: string;
  discordReadChannels: string;
  discordWriteChannels: string;
  discordVoiceTextChannel: string;
  voiceParticipation: "mentions" | "natural";
  textParticipation: "mentions" | "natural";
  participationCooldownSecs: number;
  autoStart: boolean;
  onboarded: boolean;
  sttModel: string;
  sttThreads: number;
  ttsVoice: string;
  ttsSpeed: number;
  silenceMs: number;
};

export type Job = { id: number; task: string; channelId: string; status: string };

export type CodexModel = { id: string; label: string; description: string; efforts: string[]; defaultEffort: string; supportsFast: boolean };

export type RoleConfig = { provider: string; model: string; effort: string; fast: boolean; packages: string[] };

/** The OpenAI-compatible endpoint. `models` and per-model `efforts` are whatever the provider's /models exposed (missing: use text inputs). */
export type Provider = { baseUrl: string; models: string[]; efforts: Record<string, string[]>; hasKey: boolean };

export type Stats = { turns: number; steers: number; errors: number; lastTurnMs: number; averageTurnMs: number };

export type Roles = { realtime: RoleConfig; worker: RoleConfig };

export type UpdateCheck = { current: string; latest: string; available: boolean };

type Commands = {
  codex_signed_in: boolean;
  codex_sign_in: boolean;
  codex_sign_out: boolean;
  codex_models: CodexModel[];
  get_provider: Provider;
  save_provider: Provider;
  refresh_provider_models: Provider;
  get_stats: Stats | null;
  get_roles: Roles;
  save_roles: Roles;
  get_runtime_status: RuntimeStatus;
  get_progress: string;
  check_update: UpdateCheck;
  install_update: void;
  get_voice_options: [VoiceOption[], VoiceOption[]];
  start_wallis: RuntimeStatus;
  stop_wallis: RuntimeStatus;
  get_config: WallisConfig;
  save_config: WallisConfig;
  get_secret_status: SecretStatus;
  save_discord_token: SecretStatus;
  get_profile: Profile;
  generate_wallis: Profile;
  get_jobs: Job[];
  retry_job: boolean;
};

declare global {
  interface Window {
    __TAURI__?: { core: { invoke<T>(command: string, args?: unknown): Promise<T> } };
  }
}

const fallback: WallisConfig = {
  discordApplicationId: "",
  discordAllowedUsers: "",
  discordReadChannels: "",
  discordWriteChannels: "",
  discordVoiceTextChannel: "",
  voiceParticipation: "natural",
  textParticipation: "natural",
  participationCooldownSecs: 5,
  autoStart: false,
  onboarded: false,
  sttModel: "parakeet-110m-int8",
  sttThreads: 4,
  ttsVoice: "alba",
  ttsSpeed: 1,
  silenceMs: 350,
};

export function invoke<K extends keyof Commands>(command: K, args?: unknown): Promise<Commands[K]> {
  return window.__TAURI__?.core.invoke<Commands[K]>(command, args) ?? Promise.reject(new Error("Wallis must run inside Tauri"));
}

export function defaultConfig(): WallisConfig {
  return { ...fallback };
}
