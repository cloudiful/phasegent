import { describe, expect, test } from "bun:test";
import { roleSkillDefinitions, skillDefinition } from "./src/skills.js";

const flatten = (content) => content.split(/\s+/).join(" ");
const shared = flatten(skillDefinition().content);
const rolePrompt = (id) =>
  flatten(roleSkillDefinitions().find((definition) => definition.id === `phasegent-${id}`).content);

describe("embedded parent-first decision routing (issue 768 P1)", () => {
  test("the shared skill reserves human questions and decisions for the primary", () => {
    expect(shared).toContain("## Parent-first decision routing");
    expect(shared).toContain("Only the primary orchestrator may ask the human, using the question tool");
    expect(shared).toContain("report it to the immediate parent");
    expect(shared).toContain("A denied question call is a blocker: never retry, rephrase, or use another route");
    expect(shared).toContain("no parent answers a child's native pending-question request on the human's behalf");
    expect(shared).toContain("record it in the issue body before resuming the same child with a new attempt marker");
    expect(shared).toContain("only the primary asks the human and waits for that answer");
  });

  test("each child stops and relays blockers without questioning the human", () => {
    const executor = rolePrompt("executor");
    expect(executor).toContain("report it to your immediate parent");
    expect(executor).toContain("Never ask the human through the question tool or in prose");
    expect(executor).toContain("do not retry, rephrase, or use another route");
    expect(executor).toContain("`question` is addressed to your parent, never the human");
    expect(executor).toContain("relay it to your parent rather than escalating directly");

    const reviewer = rolePrompt("reviewer");
    expect(reviewer).toContain("return `VERDICT: BLOCKED` with the existing `REVIEW` scope");
    expect(reviewer).toContain("Never ask the human through the question tool or in prose");
    expect(reviewer).toContain("A denied call is a blocker, not a reason to retry");
    expect(reviewer).toContain("address `question` to your parent");
    expect(reviewer).toContain("relay it to your parent rather than escalating directly");

    const explore = rolePrompt("explore");
    expect(explore).toContain("Never ask the human via the question tool or in prose");
    expect(explore).toContain("Return unresolved decisions and prerequisites to your immediate parent");
    expect(explore).toContain("A nested explorer returns them to its executor/reviewer parent to relay upward");
    expect(explore).toContain("the recon pointer contains no blocker status or verdict");
  });

  test("the orchestrator reads blocker detail and waits for human judgment when needed", () => {
    const orchestrator = rolePrompt("orchestrator");
    expect(orchestrator).toContain("## Resolve child blockers");
    expect(orchestrator).toContain("Read the full record body, not just its pointer");
    expect(orchestrator).toContain("Decide only within the existing user mandate and issue constraints");
    expect(orchestrator).toContain("update the issue body with it before resuming the same child");
    expect(orchestrator).toContain("only you ask the human through the question tool");
    expect(orchestrator).toContain("wait for their answer");
    expect(orchestrator).toContain("Never answer a child's native pending-question request on the human's behalf");
  });

  test("blocker handling preserves tracked, inline, and verdict contracts", () => {
    for (const status of ["DONE", "PARTIAL", "BLOCKED", "FAILED"]) {
      expect(shared).toContain(`\`${status}\``);
    }
    for (const verdict of ["PASS", "FAIL", "REQUEST_CHANGES", "BLOCKED", "AUDIT_FAILED"]) {
      expect(shared).toContain(`\`${verdict}\``);
    }
    expect(shared).toContain("A reviewer uses `VERDICT: BLOCKED` with the existing `REVIEW` scope");
    expect(shared).toContain("this is not `FAIL` or `AUDIT_FAILED`");
    expect(shared).toContain("the INLINE/LOCAL `question` field asks the parent, not the human");
    expect(shared).toContain("the minimal note-pointer JSON");
    expect(shared).toContain("Explore has no phase status or verdict");
    expect(shared).toContain("A tracked `explore` publishes an authorized recon record");
  });
});
