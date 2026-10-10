import { spawnSync } from "node:child_process";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { afterEach, describe, expect, test } from "bun:test";

const repoRoot = resolve(import.meta.dir, "../..");
const checker = join(repoRoot, "scripts/check-agent-question-permissions.mjs");
const temporaryDirs = [];

function agent(mode, rulesKey, rules) {
  const permissionText = rulesKey
    ? `${rulesKey}:\n${rules.map((rule) => `  - { action: ${rule[0]}, resource: ${JSON.stringify(rule[1])}, effect: ${rule[2]} }`).join("\n")}\n`
    : "";
  return `---\ndescription: V2 agent fixture.\nmode: ${mode}\n${permissionText}---\nPrompt body.\n`;
}

const childRules = [
  ["subagent", "*", "deny"],
  ["subagent", "explore", "allow"],
  ["question", "*", "deny"],
];
const reviewerRules = [
  ["edit", "*", "deny"],
  ["edit", "**/tests/**", "allow"],
  ["edit", "tests/**", "allow"],
  ["edit", "**/test/**", "allow"],
  ["edit", "test/**", "allow"],
  ["edit", "**/__tests__/**", "allow"],
  ["edit", "__tests__/**", "allow"],
  ["edit", "**/testdata/**", "allow"],
  ["edit", "testdata/**", "allow"],
  ["edit", "**/fixtures/**", "allow"],
  ["edit", "fixtures/**", "allow"],
  ["edit", "**/harness/**", "allow"],
  ["edit", "harness/**", "allow"],
  ["edit", "*_test.*", "allow"],
  ["edit", "*_tests.*", "allow"],
  ["edit", "*.test.*", "allow"],
  ["edit", "*.spec.*", "allow"],
  ["subagent", "*", "deny"],
  ["subagent", "explore", "allow"],
  ["question", "*", "deny"],
];

const validAgents = {
  executor: agent("subagent", "permissions", childRules),
  reviewer: agent("subagent", "permissions", reviewerRules),
  explore: agent("subagent", "permissions", [
    ["edit", "*", "deny"],
    ["subagent", "*", "deny"],
    ["question", "*", "deny"],
  ]),
  // The existing primary source relies on the host default for question and has no explicit list.
  orchestrator: agent("primary", null, []),
};

async function withAgents(overrides, run) {
  const directory = await mkdtemp(join(tmpdir(), "phasegent-agent-permissions-"));
  temporaryDirs.push(directory);
  for (const [name, content] of Object.entries({ ...validAgents, ...overrides })) {
    await writeFile(join(directory, `${name}.md`), content);
  }
  return run(directory);
}

function runChecker(directory) {
  return spawnSync(process.execPath, [checker, directory], { cwd: repoRoot, encoding: "utf8" });
}

afterEach(async () => {
  await Promise.all(temporaryDirs.splice(0).map((directory) => rm(directory, { recursive: true, force: true })));
});

describe("managed agent question permission checker", () => {
  test("parses V2 permissions lists and resolves the last question rule", async () => {
    await withAgents({}, (directory) => {
      const result = runChecker(directory);
      expect(result.status).toBe(0);
      expect(result.stdout).toContain("question permissions verified");
      expect(result.stderr).toBe("");
    });
  });

  test("rejects a missing permissions list and the wrong singular key", async () => {
    await withAgents({ executor: agent("subagent", null, []) }, (directory) => {
      const result = runChecker(directory);
      expect(result.status).toBe(1);
      expect(result.stderr).toContain("permissions must be an ordered list");
    });
    await withAgents({ executor: agent("subagent", "permission", childRules) }, (directory) => {
      const result = runChecker(directory);
      expect(result.status).toBe(1);
      expect(result.stderr).toContain("unsupported singular permission key");
    });
  });

  test("rejects a child question allow after the deny and a primary question deny", async () => {
    const childWithLaterAllow = agent("subagent", "permissions", [
      ["question", "*", "deny"],
      ["question", "*", "allow"],
    ]);
    await withAgents({ executor: childWithLaterAllow }, (directory) => {
      const result = runChecker(directory);
      expect(result.status).toBe(1);
      expect(result.stderr).toContain("last matching question rule");
    });

    const deniedPrimary = agent("primary", "permissions", [["question", "*", "deny"]]);
    await withAgents({ orchestrator: deniedPrimary }, (directory) => {
      const result = runChecker(directory);
      expect(result.status).toBe(1);
      expect(result.stderr).toContain("question must not be denied for the primary");
    });
  });

  test("requires the primary mode while leaving its question available by default", async () => {
    await withAgents({}, (directory) => {
      expect(runChecker(directory).status).toBe(0);
    });
    await withAgents({ orchestrator: agent("subagent", null, []) }, (directory) => {
      const result = runChecker(directory);
      expect(result.status).toBe(1);
      expect(result.stderr).toContain("orchestrator mode must remain primary");
    });
  });
});
