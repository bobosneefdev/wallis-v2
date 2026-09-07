import type { WallisConfig } from "./invoke";

export function validateConfig(config: WallisConfig): string | null {
  const personaName = config.personaName.trim();
  if (!/^Wallis(?:\b|$)/.test(personaName) || personaName.length > 80 || [...personaName].some((character) => { const code = character.charCodeAt(0); return code <= 0x1f || code === 0x7f; })) return "Persona name must start with Wallis and contain no control characters.";
  if (config.discordApplicationId && !/^\d{10,25}$/.test(config.discordApplicationId)) return "Discord application ID must be a numeric ID.";
  if (!/^\d{10,25}(?:,\d{10,25})*$/.test(config.discordAllowedUsers.trim())) return "Add at least one numeric Discord user ID to the allow-list.";
  return null;
}
