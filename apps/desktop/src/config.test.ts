import { describe, expect, test } from "bun:test";
import { validateConfig } from "./config";

const base = { discordApplicationId: "", discordAllowedUsers: "123456789012", personaName: "Wallis", memoryProvider: "mnemosyne" as const, autoStart: false, rootfsArchive: "", rootfsSha256: "", distroRoot: "" };

describe("validateConfig", () => {
  test("requires a Wallis persona and valid Discord ID", () => {
    expect(validateConfig({ ...base, personaName: "Ada" })).toBe("Persona name must start with Wallis and contain no control characters.");
    expect(validateConfig({ ...base, discordApplicationId: "abc" })).toBe("Discord application ID must be a numeric ID.");
    expect(validateConfig({ ...base, discordApplicationId: "123456789012" })).toBeNull();
    expect(validateConfig({ ...base, discordAllowedUsers: "" })).toBe("Add at least one numeric Discord user ID to the allow-list.");
  });
});
