// Tests for the automatic phasegent timer plugin's lifecycle, failure,
// and idempotency paths. The fake phasegent binary, client/hooks
// constructors, invocation helpers, result builders, call parsing, and
// lifecycle helpers live in `phasegent-timer-support.ts` so both this
// file and `phasegent-status.test.ts` stay under the 400-line hard cap.
//
// Status-transition mapping, skipped/disabled, and disposal tests live
// in `phasegent-status.test.ts`. Both targets call
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
  afterArgs,
  queue,
  startResponse,
} = harness;

describe("phasegent timer plugin lifecycle", () => {
  test("illegal transition surfaced through plugin throws with guidance", async () => {
    const hooks = await makeHooks();
    // Fail every invocation whose argv contains "advance": the entry
    // status advance will fail, and timer start/comment get/finish (no
    // such token) would still succeed if reached. The plugin must abort
    // before any timer is started.
    Bun.env.PHASEGENT_FAKE_EXIT = "1";
    Bun.env.PHASEGENT_FAKE_FAIL_PATTERN = "advance";
    Bun.env.PHASEGENT_FAKE_STDERR =
      '{"error":{"kind":"request","operation":"issue status advance","message":"transition rejected before any write: current status \'Resolved\' -> target status \'In Progress\' is not allowed by policy phasegent/canonical-phase-workflow@v1; allowed_next=[Closed]; recovery: phasegent --role orchestrator --provider redmine status next 51"}}';
    setFakeStdout([""]);
    const b = beforeArgs("illegal1", "executor");
    await expect(hooks["tool.execute.before"]!(b.input, b.output)).rejects.toThrow(
      /phasegent status entry failed.*transition rejected.*allowed_next=\[Closed\].*status next 51/s,
    );
    const calls = recordedCalls();
    expect(statusAdvanceCalls(calls).length).toBe(1);
    expect(calls.filter((argv) => argv.includes("timer") && argv.includes("start")).length).toBe(0);
    Bun.env.PHASEGENT_FAKE_EXIT = "0";
    Bun.env.PHASEGENT_FAKE_FAIL_PATTERN = "";
    Bun.env.PHASEGENT_FAKE_STDERR = "";
  });

  test("same-status entry advance is idempotent (server no-op allowed)", async () => {
    const notes =
      "status (DONE)\nOVERALL_CORRECTNESS: correct\n<!-- ai-executor issue=51 phase=implementation attempt=1 -->";
    const calls = await runLifecycle("idem", "executor", executorResult("DONE"), [
      { changed: false },
      startResponse("idem", "executor"),
      { notes },
      "",
      { changed: true },
    ]);
    expect(statusAdvanceCalls(calls).map(advanceTarget)).toEqual(["In Progress", "In Review"]);
  });

  test("malformed audit (missing marker) -> FAILED timer, Blocked status", async () => {
    const calls = await runLifecycle("mm1", "executor", executorResult("DONE"), [
      ENTRY_OK,
      startResponse("mm1", "executor"),
      { notes: "no marker line" },
      "",
      { changed: true },
    ]);
    expect(lastFinishResult(calls)).toBe("FAILED");
    expect(lastAdvanceTarget(calls)).toBe("Blocked");
  });

  test("malformed audit (child JSON status mismatched in note) -> FAILED", async () => {
    const calls = await runLifecycle("mm2", "executor", executorResult("DONE"), [
      ENTRY_OK,
      startResponse("mm2", "executor"),
      {
        notes:
          "status (PARTIAL)\n<!-- ai-executor issue=51 phase=implementation attempt=1 -->",
      },
      "",
      { changed: true },
    ]);
    expect(lastFinishResult(calls)).toBe("FAILED");
  });

  test("malformed audit (missing OVERALL_CORRECTNESS for reviewer) -> FAILED", async () => {
    const calls = await runLifecycle("mm3", "reviewer", reviewerResult("PASS"), [
      ENTRY_OK,
      startResponse("mm3", "reviewer"),
      { notes: "VERDICT: PASS\n<!-- ai-reviewer issue=51 phase=implementation attempt=1 -->" },
      "",
      { changed: true },
    ]);
    expect(lastFinishResult(calls)).toBe("FAILED");
  });

  test("unreadable audit (comment get returns non-JSON) -> FAILED", async () => {
    const calls = await runLifecycle("un1", "executor", executorResult("DONE"), [
      ENTRY_OK,
      startResponse("un1", "executor"),
      "not-json",
      "",
      { changed: true },
    ]);
    expect(lastFinishResult(calls)).toBe("FAILED");
  });

  test("start failure aborts via thrown error and best-effort BLOCKED finish", async () => {
    const hooks = await makeHooks();
    setFakeStdout([{ run_id: "wrong-id" }, ""]);
    const b = beforeArgs("fail1", "executor");
    await expect(hooks["tool.execute.before"]!(b.input, b.output)).rejects.toThrow(
      /phasegent timer start failed/,
    );
    const calls = recordedCalls();
    const blocked = calls.find(
      (argv) =>
        argv.includes("timer") &&
        argv.includes("finish") &&
        argv[argv.indexOf("--result") + 1] === "BLOCKED",
    );
    expect(blocked).toBeTruthy();
  });

  test("finish failure aborts via thrown error and logs", async () => {
    const hooks = await makeHooks();
    const notes =
      "status (DONE)\nOVERALL_CORRECTNESS: patch is correct\n<!-- ai-executor issue=51 phase=implementation attempt=1 -->";
    setFakeStdout(queue("ff1", "executor", notes));
    const b = beforeArgs("ff1", "executor");
    await hooks["tool.execute.before"]!(b.input, b.output);
    // Fail every subsequent invocation: timer finish + exit status.
    Bun.env.PHASEGENT_FAKE_EXIT = "7";
    Bun.env.PHASEGENT_FAKE_FAIL_PATTERN = "finish";
    Bun.env.PHASEGENT_FAKE_STDERR = "boom";
    const a = afterArgs("ff1", "executor", executorResult("DONE"));
    await expect(hooks["tool.execute.after"]!(a.input, a.output)).rejects.toThrow(/timer finish exit=7/);
    expect(harness.logs.some((l) => l.level === "error" && l.message.includes("timer finish failed"))).toBe(true);
    Bun.env.PHASEGENT_FAKE_EXIT = "0";
    Bun.env.PHASEGENT_FAKE_FAIL_PATTERN = "";
    Bun.env.PHASEGENT_FAKE_STDERR = "";
  });

  test("exit status failure aborts the after hook", async () => {
    const hooks = await makeHooks();
    const notes =
      "status (DONE)\nOVERALL_CORRECTNESS: correct\n<!-- ai-executor issue=51 phase=implementation attempt=1 -->";
    setFakeStdout(queue("ex1", "executor", notes));
    const b = beforeArgs("ex1", "executor");
    await hooks["tool.execute.before"]!(b.input, b.output);
    // Reset counter so after-hook calls index from 1 again. Queue the
    // three after-hook responses: comment get (notes), timer finish
    // (empty), exit status advance (changed:true, but will fail).
    harness.resetCallCount();
    setFakeStdout([
      { notes },
      "",
      { changed: true },
    ]);
    Bun.env.PHASEGENT_FAKE_EXIT = "11";
    Bun.env.PHASEGENT_FAKE_FAIL_PATTERN = "after:3";
    Bun.env.PHASEGENT_FAKE_STDERR =
      '{"error":{"kind":"request","operation":"issue status advance","message":"boom"}}';
    const a = afterArgs("ex1", "executor", executorResult("DONE"));
    await expect(hooks["tool.execute.after"]!(a.input, a.output)).rejects.toThrow(
      /phasegent status exit result=DONE failed/,
    );
    Bun.env.PHASEGENT_FAKE_EXIT = "0";
    Bun.env.PHASEGENT_FAKE_STDERR = "";
    Bun.env.PHASEGENT_FAKE_FAIL_PATTERN = "";
  });

  test("duplicate before hook for the same callID does not start a second run", async () => {
    const hooks = await makeHooks();
    setFakeStdout([
      ENTRY_OK,
      startResponse("dup1", "executor"),
      ENTRY_OK,
      startResponse("dup1", "executor"),
    ]);
    const b = beforeArgs("dup1", "executor");
    await hooks["tool.execute.before"]!(b.input, b.output);
    await hooks["tool.execute.before"]!(b.input, b.output);
    expect(
      recordedCalls().filter((argv) => argv.includes("timer") && argv.includes("start")).length,
    ).toBe(1);
  });
});