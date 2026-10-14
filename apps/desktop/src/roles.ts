import type { CodexModel, RoleConfig } from "./invoke";

export const CODEX_PROVIDER = "openai-codex";

/** Levels for a hand-configured provider, where no catalog says which ones the model accepts. */
export const MANUAL_EFFORTS = ["off", "minimal", "low", "medium", "high", "xhigh", "max"];

/** Selecting a catalog model keeps the current level and Fast setting only where that model supports them. */
export function chooseModel(role: RoleConfig, model: CodexModel): RoleConfig {
  const effort = model.efforts.includes(role.effort) ? role.effort : model.defaultEffort;
  return { provider: CODEX_PROVIDER, model: model.id, effort, fast: role.fast && model.supportsFast };
}

/** The catalog entry behind a role's current choice, if the role uses the signed-in Codex provider. */
export function selectedModel(role: RoleConfig, models: CodexModel[]): CodexModel | undefined {
  return role.provider === CODEX_PROVIDER ? models.find((model) => model.id === role.model) : undefined;
}
