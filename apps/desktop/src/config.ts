import type { WallisConfig } from "./invoke";

export function validateConfig(config: WallisConfig): string | null {
  if (!/^Wallis(?:\b|$)/.test(config.personaName.trim())) return "Persona name must start with Wallis.";
  if (config.discordApplicationId && !/^\d{10,25}$/.test(config.discordApplicationId)) return "Discord application ID must be a numeric ID.";
  return null;
}
