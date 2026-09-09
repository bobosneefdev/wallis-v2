import { describe, expect, test } from "bun:test";
import { validateConfig } from "./config";
import { defaultConfig } from "./invoke";

const base = { ...defaultConfig(), discordAllowedUsers: "123456789012" };

describe("validateConfig", () => {
  test("requires valid Discord IDs", () => {
    expect(validateConfig({ ...base, discordApplicationId: "abc" })).toBe("Discord application ID must be a numeric ID.");
    expect(validateConfig({ ...base, discordApplicationId: "123456789012" })).toBeNull();
    expect(validateConfig({ ...base, discordAllowedUsers: "*" })).toBeNull();
    expect(validateConfig({ ...base, discordAllowedUsers: "" })).toBe("Add Discord user IDs or * for anyone to the allow-list.");
  });
  test("limits Discord channels, output routing and participation settings", () => {
    expect(validateConfig(base)).toBeNull();
    expect(validateConfig({ ...base, discordReadChannels: "general" })).not.toBeNull();
    expect(validateConfig({ ...base, discordWriteChannels: "123456789012", discordVoiceTextChannel: "999999999999" })).not.toBeNull();
    expect(validateConfig({ ...base, discordWriteChannels: "123456789012", discordVoiceTextChannel: "123456789012" })).toBeNull();
    expect(validateConfig({ ...base, participationCooldownSecs: 0 })).not.toBeNull();
    expect(validateConfig({ ...base, participationCooldownSecs: 5.5 })).not.toBeNull();
  });
});
