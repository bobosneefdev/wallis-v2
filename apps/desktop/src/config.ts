import type { WallisConfig } from "./invoke";

export function validateConfig(config: WallisConfig): string | null {
  if (config.discordApplicationId && !/^\d{10,25}$/.test(config.discordApplicationId)) return "Discord application ID must be a numeric ID.";
  if (!/^(?:\*|\d{10,25})(?:,(?:\*|\d{10,25}))*$/.test(config.discordAllowedUsers.trim())) return "Add Discord user IDs or * for anyone to the allow-list.";
  for (const channels of [config.discordReadChannels, config.discordWriteChannels]) {
    if (channels && !/^\d{10,25}(?:,\d{10,25})*$/.test(channels)) return "Channel lists must contain numeric Discord channel IDs separated by commas.";
  }
  if (config.discordVoiceTextChannel && !config.discordWriteChannels.split(",").includes(config.discordVoiceTextChannel)) return "The voice output channel must be in the write channel list.";
  if (![config.voiceParticipation, config.textParticipation].every((mode) => mode === "mentions" || mode === "natural")) return "Choose a supported participation mode.";
  if (!Number.isInteger(config.participationCooldownSecs) || config.participationCooldownSecs < 5 || config.participationCooldownSecs > 300) return "Use 5–300 seconds between unprompted contributions.";
  return null;
}
