// Tests for the automatic phasegent plugin's status-transition mapping,
// skipped/disabled branches, and disposal path. The fake phasegent
// binary, client/hooks constructors, invocation helpers, result
// builders, call parsing, and lifecycle helpers live in
// `phasegent-timer-support.ts` so this target stays under the 400-line
// hard cap.
//
// Timer lifecycle/failure/idempotency tests live in
// `phasegent-timer.test.ts`. Both targets call
// `setupPhasegentTimerHarness()` to register their own isolated
// beforeAll/afterAll/beforeEach hooks against the shared factory.

import { describe, expect, test } from "bun:test";
import { ENTRY_OK, setupPhasegentTimerHarness } from "./phasegent-timer-support.ts";

const harness = setupPhasegentTimerHarness();
const {
  executorResult,
  reviewerResult,
  runLifecycle,
  lastAdvanceTarget,
  lastFinishResult,
  statusAdvanceCalls,
  advanceTarget,
  recordedCalls,
  setFakeStdout,
  makeHooks,
  beforeArgs,
  queue,
  startResponse,
} = harness;

describe("phasegent plugin status-transition mapping", () => {
  test("executor DONE: entry -> In Progress, exit -> In Review; finish DONE", async () => {
    const notes =
      "status (DONE)\nOVERALL_CORRECTNESS: patch is correct\n<!-- ai-executor issue=51 phase=implementation attempt=1 -->";
    const calls = await runLifecycle("c1", "executor", executorResult("DONE"), queue("c1", "executor", notes));
    expect(statusAdvanceCalls(calls).map(advanceTarget)).toEqual(["In Progress", "In Review"]);
    expect(lastFinishResult(calls)).toBe("DONE");
  });

  test("reviewer PASS maps to DONE timer and Resolved status", async () => {
    const notes =
      "VERDICT: PASS\nFINDING_COUNTS: 0/0/0/0\nOVERALL_CORRECTNESS: correct\n<!-- ai-reviewer issue=51 phase=implementation attempt=1 -->";
    const calls = await runLifecycle("r1", "reviewer", reviewerResult("PASS"), queue("r1", "reviewer", notes));
    expect(statusAdvanceCalls(calls).map(advanceTarget)).toEqual(["In Review", "Resolved"]);
    expect(lastFinishResult(calls)).toBe("DONE");
  });

  test.each([
    ["executor", "DONE", "In Review"],
    ["executor", "PARTIAL", "In Review"],
    ["executor", "BLOCKED", "Blocked"],
    ["executor", "FAILED", "Blocked"],
    ["reviewer", "PASS", "Resolved"],
    ["reviewer", "FAIL", "Changes Requested"],
    ["reviewer", "REQUEST_CHANGES", "Changes Requested"],
    ["reviewer", "BLOCKED", "Blocked"],
  ] as const)("%s %s -> %s", async (role, source, expected) => {
    const isReviewer = role === "reviewer";
    const childOutput = isReviewer
      ? reviewerResult(source === "PASS" ? "PASS" : source === "BLOCKED" ? "BLOCKED" : "FAIL")
      : executorResult(source);
    const verdictLabel = source === "PASS" ? "PASS" : source === "BLOCKED" ? "BLOCKED" : "FAIL";
    const callID = `${role}-${source}`;
    const notes = isReviewer
      ? `VERDICT: ${verdictLabel}\nOVERALL_CORRECTNESS: stub\n<!-- ai-reviewer issue=51 phase=implementation attempt=1 -->`
      : `status (${source})\nOVERALL_CORRECTNESS: stub\n<!-- ai-executor issue=51 phase=implementation attempt=1 -->`;
    const calls = await runLifecycle(callID, role, childOutput, queue(callID, role, notes));
    expect(lastAdvanceTarget(calls)).toBe(expected);
  });

  test("reviewer AUDIT_FAILED (verdict missing in note) -> Blocked", async () => {
    // AUDIT_FAILED in the JSON claim but the published note does not
    // echo it on a VERDICT: line: the plugin fails the audit-validation
    // step and maps to FAILED timer / Blocked status.
    const notes =
      "OVERALL_CORRECTNESS: incomplete\n<!-- ai-reviewer issue=51 phase=implementation attempt=1 -->";
    const calls = await runLifecycle(
      "reviewer-AUDIT_FAILED",
      "reviewer",
      reviewerResult("AUDIT_FAILED"),
      queue("reviewer-AUDIT_FAILED", "reviewer", notes),
    );
    expect(lastFinishResult(calls)).toBe("FAILED");
    expect(lastAdvanceTarget(calls)).toBe("Blocked");
  });
});

describe("phasegent plugin skipped/disabled branches", () => {
  test("background task is skipped", async () => {
    const hooks = await makeHooks();
    const b = beforeArgs("bg1", "executor", { background: true });
    await hooks["tool.execute.before"]!(b.input, b.output);
    expect(recordedCalls().length).toBe(0);
  });

  test("non-Redmine prompt is skipped", async () => {
    const hooks = await makeHooks();
    const b = beforeArgs("nr1", "executor", {
      prompt: "PHASEGENT_CONTEXT: issue=51 phase=implementation attempt=1",
    });
    await hooks["tool.execute.before"]!(b.input, b.output);
    expect(recordedCalls().length).toBe(0);
  });

  test("prompt without explicit context is skipped", async () => {
    const hooks = await makeHooks();
    const b = beforeArgs("nc1", "executor", {
      prompt: "Tracking mode is REDMINE_ISSUE\nDelegate without context.",
    });
    await hooks["tool.execute.before"]!(b.input, b.output);
    expect(recordedCalls().length).toBe(0);
  });

  test("explore and general subagents are skipped", async () => {
    const hooks = await makeHooks();
    const p = "REDMINE_ISSUE\nPHASEGENT_CONTEXT: issue=51 phase=implementation attempt=1";
    const b1 = beforeArgs("ex1", "explore", { prompt: p });
    await hooks["tool.execute.before"]!(b1.input, b1.output);
    const b2 = beforeArgs("gn1", "general", { prompt: p });
    await hooks["tool.execute.before"]!(b2.input, b2.output);
    expect(recordedCalls().length).toBe(0);
  });

  test("PHASEGENT_AUTO_TIMER=off disables automatic wrapping", async () => {
    Bun.env.PHASEGENT_AUTO_TIMER = "off";
    const hooks = await makeHooks();
    const b = beforeArgs("off1", "executor");
    await hooks["tool.execute.before"]!(b.input, b.output);
    expect(recordedCalls().length).toBe(0);
    expect(
      harness.logs.some((l) => l.level === "debug" && l.message.includes("automatic timer disabled")),
    ).toBe(true);
  });
});

describe("phasegent plugin disposal", () => {
  test("graceful dispose finishes in-flight runs as FAILED", async () => {
    const hooks = await makeHooks();
    setFakeStdout([
      ENTRY_OK,
      startResponse("disp1", "executor"),
      "",
    ]);
    const b = beforeArgs("disp1", "executor");
    await hooks["tool.execute.before"]!(b.input, b.output);
    await hooks.dispose!();
    const calls = recordedCalls();
    const finishes = calls.filter((argv) => argv.includes("finish"));
    expect(finishes.length).toBeGreaterThan(0);
    expect(finishes[finishes.length - 1][finishes[finishes.length - 1].indexOf("--result") + 1]).toBe("FAILED");
  });
});