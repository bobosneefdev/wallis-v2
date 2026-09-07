import type { WallisConfig } from "./invoke";

export function validateConfig(config: WallisConfig): string | null {
  if (config.discordApplicationId && !/^\d{10,25}$/.test(config.discordApplicationId)) return "Discord application ID must be a numeric ID.";
  if (!/^\d{10,25}(?:,\d{10,25})*$/.test(config.discordAllowedUsers.trim())) return "Add at least one numeric Discord user ID to the allow-list.";
  return null;
}
