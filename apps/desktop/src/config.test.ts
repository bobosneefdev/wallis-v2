import { describe, expect, test } from "bun:test";
import { validateConfig } from "./config";
import { defaultConfig } from "./invoke";

const base = { ...defaultConfig(), discordAllowedUsers: "123456789012" };

describe("validateConfig", () => {
  test("requires valid Discord IDs", () => {
    expect(validateConfig({ ...base, discordApplicationId: "abc" })).toBe("Discord application ID must be a numeric ID.");
    expect(validateConfig({ ...base, discordApplicationId: "123456789012" })).toBeNull();
    expect(validateConfig({ ...base, discordAllowedUsers: "" })).toBe("Add at least one numeric Discord user ID to the allow-list.");
  });
});
