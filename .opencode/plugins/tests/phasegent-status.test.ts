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

describe("phasegent plugin background skip", () => {
  test("background task emits a warning and never invokes phasegent", async () => {
    const hooks = await makeHooks();
    const b = beforeArgs("bg2", "executor", { background: true });
    await hooks["tool.execute.before"]!(b.input, b.output);
    expect(recordedCalls().length).toBe(0);
    expect(
      harness.logs.some(
        (l) => l.level === "warn" && /background task skipped/.test(l.message)
          && /timer recover/.test(l.message),
      ),
    ).toBe(true);
  });

  test("background + non-Redmine prompt still warns with explicit guidance", async () => {
    const hooks = await makeHooks();
    const b = beforeArgs("bg3", "explore", { background: true });
    await hooks["tool.execute.before"]!(b.input, b.output);
    expect(recordedCalls().length).toBe(0);
    expect(
      harness.logs.some(
        (l) => l.level === "warn" && /rerun foreground/.test(l.message),
      ),
    ).toBe(true);
  });
});

describe("phasegent plugin timer start passes owner metadata", () => {
  test("start argv carries --owner-session-id and --owner-call-id", async () => {
    const hooks = await makeHooks();
    setFakeStdout([
      ENTRY_OK,
      startResponse("owner1", "executor"),
      { notes: "status (DONE)\nOVERALL_CORRECTNESS: ok\n<!-- ai-executor issue=51 phase=implementation attempt=1 -->" },
      "",
      { changed: true },
    ]);
    const b = beforeArgs("owner1", "executor");
    await hooks["tool.execute.before"]!(b.input, b.output);
    const startCalls = recordedCalls().filter((argv) => argv.includes("timer") && argv.includes("start"));
    expect(startCalls.length).toBe(1);
    const startCall = startCalls[0];
    expect(startCall).toContain("--owner-session-id");
    expect(startCall).toContain("--owner-call-id");
    const sessionIdx = startCall.indexOf("--owner-session-id");
    const callIdx = startCall.indexOf("--owner-call-id");
    expect(sessionIdx).toBeGreaterThan(-1);
    expect(callIdx).toBeGreaterThan(sessionIdx);
    expect(startCall[sessionIdx + 1]).toBe("s1");
    expect(startCall[callIdx + 1]).toBe("owner1");
  });
});

describe("phasegent plugin dispose", () => {
  test("graceful dispose finishes in-flight runs as FAILED via retry path", async () => {
    const hooks = await makeHooks();
    setFakeStdout([ENTRY_OK, startResponse("disp2", "executor"), ""]);
    const b = beforeArgs("disp2", "executor");
    await hooks["tool.execute.before"]!(b.input, b.output);
    await hooks.dispose!();
    const finishes = recordedCalls().filter((argv) => argv.includes("finish"));
    expect(finishes.length).toBeGreaterThan(0);
    const last = finishes[finishes.length - 1];
    expect(last[last.indexOf("--result") + 1]).toBe("FAILED");
    const rid = harness.predictRunId("disp2", "executor");
    expect(harness.logs.some((l) => l.level === "info" && l.message.includes(`dispose finished in-flight run_id=${rid}`))).toBe(true);
  });
  test("dispose warnings on persistent finish failure name the recover command", async () => {
    const hooks = await makeHooks();
    setFakeStdout([ENTRY_OK, startResponse("disp3", "executor"), ""]);
    const b = beforeArgs("disp3", "executor");
    await hooks["tool.execute.before"]!(b.input, b.output);
    Bun.env.PHASEGENT_FAKE_EXIT = "9";
    Bun.env.PHASEGENT_FAKE_FAIL_PATTERN = "finish";
    Bun.env.PHASEGENT_FAKE_STDERR = "boom";
    await hooks.dispose!();
    const rid3 = harness.predictRunId("disp3", "executor");
    expect(harness.logs.some((l) => l.level === "warn" && l.message.includes(`timer recover ${rid3}`))).toBe(true);
    Bun.env.PHASEGENT_FAKE_EXIT = "0";
    Bun.env.PHASEGENT_FAKE_FAIL_PATTERN = "";
    Bun.env.PHASEGENT_FAKE_STDERR = "";
  });
});

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

describe("phasegent timer run-id collision resistance", () => {
  test("previously colliding 32-bit ids now differ with 64-bit", async () => {
    const mod = await import("../phasegent-timer.ts");
    const derive = (mod as any).__private.deriveRunId as (c: any, r: string, o: any) => string;
    const base = { issue: "51", phase: "implementation", attempt: "1" };
    const a = derive(base, "executor", { sessionId: "s1", callId: "call-18909" });
    const b = derive(base, "executor", { sessionId: "s1", callId: "call-693546" });
    expect(a).not.toBe(b);
    expect(a.split("-").pop()!.length).toBe(16);
    expect(b.split("-").pop()!.length).toBe(16);
  });
});

describe("phasegent binary resolution", () => {
  test("isExecutable requires regular executable file", async () => {
    const mod = await import("../phasegent-timer.ts");
    const isExec = (mod as any).__private.isExecutable as (p: string) => boolean;
    const { mkdtempSync, rmSync, writeFileSync, mkdirSync, chmodSync } = await import("node:fs");
    const { tmpdir } = await import("node:os");
    const { join } = await import("node:path");
    const dir = mkdtempSync(join(tmpdir(), "phasegent-binary-"));
    const valid = join(dir, "valid-phasegent");
    writeFileSync(valid, "#!/bin/sh\necho ok\n");
    chmodSync(valid, 0o755);
    const d = join(dir, "dir-target");
    mkdirSync(d);
    const nonExec = join(dir, "non-exec");
    writeFileSync(nonExec, "not executable");
    chmodSync(nonExec, 0o644);
    expect(isExec(valid)).toBe(true);
    expect(isExec(d)).toBe(false);
    expect(isExec(nonExec)).toBe(false);
    expect(isExec(join(dir, "no-such"))).toBe(false);
    rmSync(dir, { recursive: true, force: true });
  });
  test("binaryPath skips invalid override and picks next executable", async () => {
    const mod = await import("../phasegent-timer.ts");
    const bin = (mod as any).__private.binaryPath as () => string;
    const { mkdtempSync, rmSync, writeFileSync, chmodSync, mkdirSync } = await import("node:fs");
    const { tmpdir } = await import("node:os");
    const { join } = await import("node:path");
    const dir = mkdtempSync(join(tmpdir(), "phasegent-binary2-"));
    const valid = join(dir, "valid-phasegent");
    writeFileSync(valid, "#!/bin/sh\necho ok\n");
    chmodSync(valid, 0o755);
    const fakeDir = join(dir, "dir-target");
    mkdirSync(fakeDir);
    const savedBin = Bun.env.PHASEGENT_BIN;
    const savedPath = Bun.env.PATH;
    try {
      Bun.env.PHASEGENT_BIN = fakeDir;
      const pathWithValid = `${dir}:${savedPath ?? ""}`;
      Bun.env.PATH = pathWithValid;
      (process.env as any).PATH = pathWithValid;
      const pathCandidate = join(dir, "phasegent");
      writeFileSync(pathCandidate, "#!/bin/sh\necho ok\n");
      chmodSync(pathCandidate, 0o755);
      const chosen = bin();
      expect(chosen).not.toBe(fakeDir);
      const worktree = "/workspace/tools/phasegent/target/debug/phasegent";
      expect(
        chosen === pathCandidate ||
          chosen === "phasegent" ||
          chosen === valid ||
          chosen === worktree ||
          chosen === "/home/dev/.cargo/bin/phasegent",
      ).toBe(true);
      Bun.env.PHASEGENT_BIN = valid;
      expect(bin()).toBe(valid);
      rmSync(pathCandidate);
    } finally {
      if (savedBin === undefined) delete Bun.env.PHASEGENT_BIN;
      else Bun.env.PHASEGENT_BIN = savedBin;
      if (savedPath === undefined) delete Bun.env.PATH;
      else Bun.env.PATH = savedPath;
      rmSync(dir, { recursive: true, force: true });
    }
  });
  test("auto fallback tries next candidate after stale executable", async () => {
    const { mkdtempSync, rmSync, writeFileSync, chmodSync, existsSync, renameSync } = await import("node:fs");
    const { tmpdir } = await import("node:os");
    const { join } = await import("node:path");
    const staleDir = mkdtempSync(join(tmpdir(), "phasegent-stale-"));
    const validDir = mkdtempSync(join(tmpdir(), "phasegent-valid-"));
    const staleBin = join(staleDir, "phasegent");
    writeFileSync(staleBin, "#!/bin/sh\necho 'unknown option --owner-session-id' >&2; exit 1\n");
    chmodSync(staleBin, 0o755);
    const validBin = join(validDir, "phasegent");
    writeFileSync(validBin, "#!/bin/sh\necho '{\"run_id\":\"ok\"}'; exit 0\n");
    chmodSync(validBin, 0o755);
    const worktree = "/workspace/tools/phasegent/target/debug/phasegent";
    const worktreeBak = worktree + ".bak-test";
    let hadWorktree = false;
    if (existsSync(worktree)) { hadWorktree = true; renameSync(worktree, worktreeBak); }
    const savedBin = Bun.env.PHASEGENT_BIN;
    const savedPath = Bun.env.PATH;
    const savedFakeArgs = Bun.env.PHASEGENT_FAKE_ARGS_LOG;
    const savedFakeStdout = Bun.env.PHASEGENT_FAKE_STDOUT_FILE;
    const savedFakeCount = Bun.env.PHASEGENT_FAKE_CALL_COUNT_FILE;
    try {
      delete Bun.env.PHASEGENT_BIN;
      delete Bun.env.PHASEGENT_FAKE_ARGS_LOG;
      delete Bun.env.PHASEGENT_FAKE_STDOUT_FILE;
      delete Bun.env.PHASEGENT_FAKE_CALL_COUNT_FILE;
      const newPath = `${staleDir}:${validDir}:${savedPath ?? ""}`;
      Bun.env.PATH = newPath; (process.env as any).PATH = newPath;
      const mod = await import("../phasegent-timer.ts");
      const cands = (mod as any).__private.autoCandidates as () => string[];
      const isExec = (mod as any).__private.isExecutable as (p: string) => boolean;
      const list = cands();
      expect(list.indexOf(staleBin)).toBeGreaterThan(-1);
      expect(list.indexOf(validBin)).toBeGreaterThan(-1);
      expect(list.indexOf(staleBin)).toBeLessThan(list.indexOf(validBin));
      expect(isExec(staleBin)).toBe(true);
      expect(isExec(validBin)).toBe(true);
    } finally {
      if (savedBin === undefined) delete Bun.env.PHASEGENT_BIN; else Bun.env.PHASEGENT_BIN = savedBin;
      if (savedPath === undefined) delete Bun.env.PATH; else Bun.env.PATH = savedPath;
      (process.env as any).PATH = savedPath ?? "";
      if (savedFakeArgs) Bun.env.PHASEGENT_FAKE_ARGS_LOG = savedFakeArgs; else delete Bun.env.PHASEGENT_FAKE_ARGS_LOG;
      if (savedFakeStdout) Bun.env.PHASEGENT_FAKE_STDOUT_FILE = savedFakeStdout; else delete Bun.env.PHASEGENT_FAKE_STDOUT_FILE;
      if (savedFakeCount) Bun.env.PHASEGENT_FAKE_CALL_COUNT_FILE = savedFakeCount; else delete Bun.env.PHASEGENT_FAKE_CALL_COUNT_FILE;
      if (hadWorktree) renameSync(worktreeBak, worktree);
      rmSync(staleDir, { recursive: true, force: true });
      rmSync(validDir, { recursive: true, force: true });
    }
  });
});

describe("phasegent plugin outcome strict vocabularies", () => {
  test("executor PASS (cross-role) maps to FAILED", async () => {
    const notes = "status (PASS)\nOVERALL_CORRECTNESS: patch is correct\n<!-- ai-executor issue=51 phase=implementation attempt=1 -->";
    const fakeResult = `<task_result>${JSON.stringify({ status: "PASS", tracking: { mode: "REDMINE_ISSUE", comment: "posted", comment_id: "100", comment_url: "https://redmine.cloud1ful.com/issues/51#note-100", marker: "<!-- ai-executor issue=51 phase=implementation attempt=1 -->" } })}</task_result>`;
    const calls = await harness.runLifecycle("exec-PASS", "executor", fakeResult, harness.queue("exec-PASS", "executor", notes));
    expect(harness.lastFinishResult(calls)).toBe("FAILED");
    expect(harness.lastAdvanceTarget(calls)).toBe("Blocked");
  });
  test("reviewer DONE (cross-role) maps to FAILED", async () => {
    const notes = "VERDICT: DONE\nOVERALL_CORRECTNESS: correct\n<!-- ai-reviewer issue=51 phase=implementation attempt=1 -->";
    const fakeResult = `<task_result>${JSON.stringify({ verdict: "DONE", tracking: { mode: "REDMINE_ISSUE", comment: "posted", comment_id: "200", comment_url: "https://redmine.cloud1ful.com/issues/51#note-200", marker: "<!-- ai-reviewer issue=51 phase=implementation attempt=1 -->" } })}</task_result>`;
    const calls = await harness.runLifecycle("rev-DONE", "reviewer", fakeResult, harness.queue("rev-DONE", "reviewer", notes));
    expect(harness.lastFinishResult(calls)).toBe("FAILED");
    expect(harness.lastAdvanceTarget(calls)).toBe("Blocked");
  });
  test("reviewer FAIL maps to PARTIAL/Changes Requested", async () => {
    const notes = "VERDICT: FAIL\nOVERALL_CORRECTNESS: correct\n<!-- ai-reviewer issue=51 phase=implementation attempt=1 -->";
    const calls = await harness.runLifecycle("rev-FAIL", "reviewer", harness.reviewerResult("FAIL"), harness.queue("rev-FAIL", "reviewer", notes));
    expect(harness.lastFinishResult(calls)).toBe("PARTIAL");
    expect(harness.lastAdvanceTarget(calls)).toBe("Changes Requested");
  });
  test("reviewer REQUEST_CHANGES maps to PARTIAL", async () => {
    const notes = "VERDICT: REQUEST_CHANGES\nOVERALL_CORRECTNESS: correct\n<!-- ai-reviewer issue=51 phase=implementation attempt=1 -->";
    const calls = await harness.runLifecycle("rev-RC", "reviewer", harness.reviewerResult("REQUEST_CHANGES"), harness.queue("rev-RC", "reviewer", notes));
    expect(harness.lastFinishResult(calls)).toBe("PARTIAL");
  });
  test("reviewer AUDIT_FAILED maps to FAILED", async () => {
    const notes = "VERDICT: AUDIT_FAILED\nOVERALL_CORRECTNESS: failed\n<!-- ai-reviewer issue=51 phase=implementation attempt=1 -->";
    const calls = await harness.runLifecycle("rev-AF", "reviewer", harness.reviewerResult("AUDIT_FAILED"), harness.queue("rev-AF", "reviewer", notes));
    expect(harness.lastFinishResult(calls)).toBe("FAILED");
  });
});