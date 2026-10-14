import { describe, expect, test } from "bun:test";
import type { CodexModel } from "./invoke";
import { chooseModel, packageError, parsePackages, selectedModel } from "./roles";

const sol: CodexModel = { id: "gpt-6.1-sol", label: "Sol", description: "", efforts: ["low", "medium", "high", "ultra"], defaultEffort: "medium", supportsFast: true };
const luna: CodexModel = { id: "gpt-6-luna", label: "Luna", description: "", efforts: ["low", "medium"], defaultEffort: "low", supportsFast: false };
const role = { provider: "openai-codex", model: sol.id, effort: "ultra", fast: true, packages: ["npm:x"] };

describe("chooseModel", () => {
  test("drops a level or Fast mode the new model does not offer, keeps packages", () => {
    expect(chooseModel(role, luna)).toEqual({ provider: "openai-codex", model: luna.id, effort: "low", fast: false, packages: ["npm:x"] });
  });
  test("keeps supported settings and switches a custom provider to Codex", () => {
    expect(chooseModel({ ...role, provider: "custom" }, sol)).toEqual(role);
  });
});

describe("selectedModel", () => {
  test("only resolves for the Codex provider", () => {
    expect(selectedModel(role, [sol, luna])).toBe(sol);
    expect(selectedModel({ ...role, provider: "custom" }, [sol])).toBeUndefined();
  });
});

describe("packages", () => {
  test("one source per line, blanks dropped", () => {
    expect(parsePackages(" npm:a \n\n git:github.com/u/r \n")).toEqual(["npm:a", "git:github.com/u/r"]);
  });
  test("rejects local paths, flags and spaces", () => {
    expect(packageError(["npm:a", "https://github.com/u/r"])).toBeNull();
    for (const bad of ["--flag", "/etc/passwd", "npm: a"]) expect(packageError([bad])).not.toBeNull();
  });
});
