import type { CodexModel, RoleConfig } from "./invoke";

export const CODEX_PROVIDER = "openai-codex";
/** Pi provider name of the OpenAI-compatible endpoint (must match `pi_config::CUSTOM`). */
export const CUSTOM_PROVIDER = "custom";

/** Selecting a catalog model keeps the current level and Fast setting only where that model supports them. */
export function chooseModel(role: RoleConfig, model: CodexModel): RoleConfig {
  const effort = model.efforts.includes(role.effort) ? role.effort : model.defaultEffort;
  return { ...role, provider: CODEX_PROVIDER, model: model.id, effort, fast: role.fast && model.supportsFast };
}

/** The catalog entry behind a role's current choice, if the role uses the signed-in Codex provider. */
export function selectedModel(role: RoleConfig, models: CodexModel[]): CodexModel | undefined {
  return role.provider === CODEX_PROVIDER ? models.find((model) => model.id === role.model) : undefined;
}

/** One Pi package source per line. */
export const parsePackages = (text: string): string[] => text.split("\n").map((line) => line.trim()).filter(Boolean);

/** Mirrors the backend rule: registry, git or https sources, no spaces. */
export const packageError = (packages: string[]): string | null => {
  const bad = packages.find((source) => !/^(npm:|git:|https:\/\/)\S+$/.test(source) || source.length > 200);
  return bad ? `"${bad}" must start with npm:, git: or https:// and contain no spaces.` : null;
};
