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

describe("embedded acceptance fidelity obligations (issue 109 P1)", () => {
  test("the executor keeps explicitly named requirements and reports deviations", () => {
    const executor = rolePrompt("executor");
    expect(executor).toContain("## Follow the phase implementation contract");
    expect(executor).toContain(
      "Meet the explicit requirements the phase names — the named control, interface, format, placement, and identity — exactly as stated",
    );
    expect(executor).toContain(
      "a functionally equivalent substitute is a deviation, not an executor choice",
    );
    expect(executor).toContain(
      "When a required control, interface, format, or placement cannot be met as stated, report the deviation to your parent instead of shipping a substitute",
    );
  });

  test("the reviewer maps acceptance criteria to observed evidence and checks identity and placement", () => {
    const reviewer = rolePrompt("reviewer");
    expect(reviewer).toContain(
      "Map each acceptance criterion to the evidence that covers it and the outcome that evidence showed",
    );
    expect(reviewer).toContain(
      "group criteria only when one shared evidence set covers each of them",
    );
    expect(reviewer).toContain(
      "Check the identity and placement an acceptance criterion names, not only the behavior",
    );
    expect(reviewer).toContain(
      "a functionally equivalent substitute for an explicitly required control, interface, format, or placement is a mismatch",
    );
    expect(reviewer).toContain(
      "Treat a green suite as evidence for what it covered, never for a criterion you did not observe",
    );
    expect(reviewer).toContain("required evidence that is missing blocks `PASS`");
    expect(reviewer).toContain(
      "closes the acceptance coverage gap the orchestrator routed back",
    );
  });

  test("the orchestrator checks audit coverage before accepting a PASS or opening delivery", () => {
    const orchestrator = rolePrompt("orchestrator");
    expect(orchestrator).toContain(
      "check that the published audit covers the issue's current acceptance criteria",
    );
    expect(orchestrator).toContain(
      "a coverage gap goes back to the same reviewer on the same artifact",
    );
    expect(orchestrator).toContain("this never adds a routine extra round");
    expect(orchestrator).toContain(
      "returns `PASS` on an audit that covers the issue's current acceptance criteria",
    );
  });

  test("the new duties add no permission, round, token, or severity rule", () => {
    for (const id of ["executor", "reviewer", "orchestrator"]) {
      expect(rolePrompt(id)).not.toMatch(/\bP4\b|\bS[0-3]\b/);
    }
    // The unchanged boundaries keep holding next to the new obligations.
    expect(rolePrompt("executor")).toContain("Never commit, push, tag, or mutate refs");
    expect(rolePrompt("reviewer")).toContain("Never modify production code");
    expect(rolePrompt("reviewer")).toContain("PASS means no confirmed P0-P2");
    expect(rolePrompt("reviewer")).toContain("there is no defect-count quota");
    expect(rolePrompt("orchestrator")).toContain("requires no extra review rounds");
  });
});
