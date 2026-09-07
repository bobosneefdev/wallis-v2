import { describe, expect, test } from "bun:test";
import { validateConfig } from "./config";

const base = { discordApplicationId: "", personaName: "Wallis", memoryProvider: "mnemosyne" as const, autoStart: false };

describe("validateConfig", () => {
  test("requires a Wallis persona and valid Discord ID", () => {
    expect(validateConfig({ ...base, personaName: "Ada" })).toBe("Persona name must start with Wallis.");
    expect(validateConfig({ ...base, discordApplicationId: "abc" })).toBe("Discord application ID must be a numeric ID.");
    expect(validateConfig({ ...base, discordApplicationId: "123456789012" })).toBeNull();
  });
});
