import { describe, expect, test } from "bun:test";
import type { CodexModel } from "./invoke";
import { chooseModel, selectedModel } from "./roles";

const sol: CodexModel = { id: "gpt-6.1-sol", label: "Sol", description: "", efforts: ["low", "medium", "high", "ultra"], defaultEffort: "medium", supportsFast: true };
const luna: CodexModel = { id: "gpt-6-luna", label: "Luna", description: "", efforts: ["low", "medium"], defaultEffort: "low", supportsFast: false };
const role = { provider: "openai-codex", model: sol.id, effort: "ultra", fast: true };

describe("chooseModel", () => {
  test("drops a level or Fast mode the new model does not offer", () => {
    expect(chooseModel(role, luna)).toEqual({ provider: "openai-codex", model: luna.id, effort: "low", fast: false });
  });
  test("keeps supported settings and switches a manual provider to Codex", () => {
    expect(chooseModel({ ...role, provider: "openrouter" }, sol)).toEqual(role);
  });
});

describe("selectedModel", () => {
  test("only resolves for the Codex provider", () => {
    expect(selectedModel(role, [sol, luna])).toBe(sol);
    expect(selectedModel({ ...role, provider: "openrouter" }, [sol])).toBeUndefined();
  });
});
