// Shared support for the phasegent timer + phase status plugin tests.
//
// Provides the fake phasegent executable (an sh script that records argv
// into a log file and returns scripted stdout responses), the OpenCode
// client/hooks constructors, invocation helpers, result builders, call
// parsing, and the lifecycle helpers every test relies on. Production
// code stays untouched; only this support module owns the test-time
// setup.
//
// Each test target calls `setupPhasegentTimerHarness()` at its top level
// so the registration of `beforeAll`/`afterAll`/`beforeEach` happens in
// the importing file's scope and per-file state stays isolated. No
// module-load side effects mutate shared state: the factory closes over
// its own `tempDir`, log paths, and `logs` array, so two test files can
// run from the same Bun process without leaking env vars or file paths
// into each other.

import { afterAll, beforeAll, beforeEach } from "bun:test";
import { chmodSync, existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import type { OpencodeClient } from "@opencode-ai/sdk";
import type { Hooks, PluginInput } from "@opencode-ai/plugin";
import plugin from "../phasegent-timer.ts";

export const SESSION_ID = "s1";
const FAKE_BIN_NAME = "fake-phasegent";

export const ENTRY_OK: { changed: true } = { changed: true };

/// Stable run-id template: every test shares the issue/phase/attempt,
/// so the run id depends only on role + session + callID. Re-exported so
/// the duplicate-idempotency test can hand-author responses when needed.
export interface TimerTestHarness {
  readonly logs: ReadonlyArray<{ level: string; message: string }>;
  setFakeStdout(responses: Array<unknown>): void;
  makeClient(): OpencodeClient;
  makeHooks(): Promise<Hooks>;
  recordedCalls(): string[][];
  beforeArgs(
    callID: string,
    role: string,
    opts?: { background?: boolean; prompt?: string },
  ): { input: any; output: any };
  afterArgs(
    callID: string,
    role: string,
    output: string,
    opts?: { background?: boolean; prompt?: string },
  ): { input: any; output: any };
  runLifecycle(
    callID: string,
    role: string,
    output: string,
    responses: Array<unknown>,
  ): Promise<string[][]>;
  timerFinishCalls(calls: string[][]): string[][];
  statusAdvanceCalls(calls: string[][]): string[][];
  advanceTarget(argv: string[]): string;
  lastFinishResult(calls: string[][]): string;
  lastAdvanceTarget(calls: string[][]): string | null;
  predictRunId(callID: string, role: string): string;
  startResponse(callID: string, role: string): { run_id: string; created: boolean };
  queue(callID: string, role: string, notes: string, changed?: boolean): unknown[];
  executorResult(status: "DONE" | "PARTIAL" | "BLOCKED" | "FAILED"): string;
  reviewerResult(verdict: string): string;
  prompt(role?: string): string;
  /// Reset the per-test call counter so after-hook calls index from 1
  /// again. Used by the exit-status-failure scenario that needs two
  /// independent lifecycles inside one test.
  resetCallCount(): void;
}

export function setupPhasegentTimerHarness(): TimerTestHarness {
  let tempDir = "";
  let fakeBin = "";
  let fakeArgsLog = "";
  let fakeStdoutLog = "";
  let fakeCallCountLog = "";
  const logs: Array<{ level: string; message: string }> = [];

  beforeAll(() => {
    tempDir = mkdtempSync(join(tmpdir(), "phasegent-timer-test-"));
    fakeBin = join(tempDir, FAKE_BIN_NAME);
    fakeArgsLog = join(tempDir, "calls.args.log");
    fakeStdoutLog = join(tempDir, "calls.stdout.log");
    fakeCallCountLog = join(tempDir, "calls.count");
    // The fake phasegent records argv lines into fakeArgsLog and reads the
    // Nth newline-delimited block from fakeStdoutLog so each invocation
    // returns the right scripted response.
    const scriptLines = [
      "#!/bin/sh",
      'lockfile="$PHASEGENT_FAKE_CALL_COUNT_FILE"',
      "i=0",
      'if [ -f "$lockfile" ]; then',
      '  i=$(cat "$lockfile")',
      "fi",
      'next=$((i + 1))',
      'echo $next > "$lockfile"',
      'printf \'%s\\n\' "$0" >> "$PHASEGENT_FAKE_ARGS_LOG"',
      'for arg in "$@"; do',
      '  printf \'%s\\n\' "$arg" >> "$PHASEGENT_FAKE_ARGS_LOG"',
      "done",
      "# Selective fail modes: 'token' (argv token match) or 'after:N' (>= N).",
      "fail=0",
      'if [ -n "$PHASEGENT_FAKE_FAIL_PATTERN" ]; then',
      '  case "$PHASEGENT_FAKE_FAIL_PATTERN" in',
      "    after:*)",
      '      threshold=${PHASEGENT_FAKE_FAIL_PATTERN#after:}',
      '      if [ "$next" -ge "$threshold" ]; then fail=1; fi',
      "      ;;",
      "    *)",
      '      for arg in "$@"; do',
      '        if [ "$arg" = "$PHASEGENT_FAKE_FAIL_PATTERN" ]; then fail=1; break; fi',
      "      done",
      "      ;;",
      "  esac",
      "fi",
      'if [ "$fail" = "1" ]; then',
      '  if [ -n "$PHASEGENT_FAKE_STDERR" ]; then',
      '    printf \'%s\' "$PHASEGENT_FAKE_STDERR" >&2',
      "  fi",
      '  exit "${PHASEGENT_FAKE_EXIT:-1}"',
      "fi",
      'if [ -f "$PHASEGENT_FAKE_STDOUT_FILE" ]; then',
      "  awk -v target=$next '",
      "    BEGIN { block = 1 }",
      "    /^<<<>>>$/ { block++; next }",
      "    block == target { print; fflush() }",
      "  ' \"$PHASEGENT_FAKE_STDOUT_FILE\"",
      "fi",
      "exit 0",
      "",
    ];
    writeFileSync(fakeBin, scriptLines.join("\n"));
    chmodSync(fakeBin, 0o755);
    Bun.env.PHASEGENT_BIN = fakeBin;
    Bun.env.PHASEGENT_FAKE_ARGS_LOG = fakeArgsLog;
    Bun.env.PHASEGENT_FAKE_STDOUT_FILE = fakeStdoutLog;
    Bun.env.PHASEGENT_FAKE_CALL_COUNT_FILE = fakeCallCountLog;
  });

  afterAll(() => {
    for (const key of [
      "PHASEGENT_BIN",
      "PHASEGENT_FAKE_ARGS_LOG",
      "PHASEGENT_FAKE_STDOUT_FILE",
      "PHASEGENT_FAKE_CALL_COUNT_FILE",
      "PHASEGENT_FAKE_STDERR",
      "PHASEGENT_FAKE_EXIT",
      "PHASEGENT_FAKE_EXIT_FORCE",
      "PHASEGENT_FAKE_FAIL_PATTERN",
      "PHASEGENT_AUTO_TIMER",
    ]) {
      delete Bun.env[key];
    }
    rmSync(tempDir, { recursive: true, force: true });
  });

  beforeEach(() => {
    rmSync(fakeArgsLog, { force: true });
    rmSync(fakeStdoutLog, { force: true });
    rmSync(fakeCallCountLog, { force: true });
    writeFileSync(fakeStdoutLog, "");
    writeFileSync(fakeCallCountLog, "0");
    Bun.env.PHASEGENT_FAKE_EXIT = "0";
    Bun.env.PHASEGENT_FAKE_STDERR = "";
    Bun.env.PHASEGENT_FAKE_FAIL_PATTERN = "";
    delete Bun.env.PHASEGENT_AUTO_TIMER;
    logs.length = 0;
  });

  function makeClient(): OpencodeClient {
    return {
      app: {
        log: async ({ body }: { body: { level: string; message: string } }) => {
          logs.push({ level: body.level, message: body.message });
          return { data: true };
        },
      },
    } as unknown as OpencodeClient;
  }

  async function makeHooks(): Promise<Hooks> {
    return await plugin.server({ client: makeClient() } as unknown as PluginInput);
  }

  function recordedCalls(): string[][] {
    if (!existsSync(fakeArgsLog)) return [];
    const raw = readFileSync(fakeArgsLog, "utf8").split("\n");
    const calls: string[][] = [];
    let current: string[] | null = null;
    for (const line of raw) {
      if (line === fakeBin) {
        if (current) calls.push(current);
        current = [];
      } else if (current) {
        current.push(line);
      }
    }
    if (current) calls.push(current);
    return calls;
  }

  function setFakeStdout(responses: Array<unknown>): void {
    const blocks = responses.map((entry) =>
      typeof entry === "string" ? entry : JSON.stringify(entry),
    );
    writeFileSync(fakeStdoutLog, blocks.map((b) => `${b}\n<<<>>>`).join("\n"));
  }

  function prompt(role = "executor"): string {
    return [
      "Tracking mode is REDMINE_ISSUE",
      "PHASEGENT_CONTEXT: issue=51 phase=implementation attempt=1",
      `Delegate to ${role}.`,
    ].join("\n");
  }

  function executorResult(status: "DONE" | "PARTIAL" | "BLOCKED" | "FAILED"): string {
    const body = JSON.stringify({
      status,
      tracking: {
        mode: "REDMINE_ISSUE",
        comment: "posted",
        comment_id: "100",
        comment_url: "https://redmine.cloud1ful.com/issues/51#note-100",
        marker: "<!-- ai-executor issue=51 phase=implementation attempt=1 -->",
      },
    });
    return `<task_result>${body}</task_result>`;
  }

  function reviewerResult(verdict: string): string {
    const body = JSON.stringify({
      verdict,
      tracking: {
        mode: "REDMINE_ISSUE",
        comment: "posted",
        comment_id: "200",
        comment_url: "https://redmine.cloud1ful.com/issues/51#note-200",
        marker: "<!-- ai-reviewer issue=51 phase=implementation attempt=1 -->",
      },
    });
    return `<task_result>${body}</task_result>`;
  }

  function taskArgs(taskPrompt: string, role: string, background = false): unknown {
    return { description: `delegate ${role}`, prompt: taskPrompt, subagent_type: role, background };
  }

  function beforeArgs(
    callID: string,
    role: string,
    opts: { background?: boolean; prompt?: string } = {},
  ): { input: any; output: any } {
    const args = taskArgs(opts.prompt ?? prompt(role), role, opts.background ?? false);
    return { input: { tool: "task", sessionID: SESSION_ID, callID } as never, output: { args } as never };
  }

  function afterArgs(
    callID: string,
    role: string,
    output: string,
    opts: { background?: boolean; prompt?: string } = {},
  ): { input: any; output: any } {
    const args = taskArgs(opts.prompt ?? prompt(role), role, opts.background ?? false);
    return {
      input: { tool: "task", sessionID: SESSION_ID, callID, args } as never,
      output: { title: "job", output, metadata: {} } as never,
    };
  }

  /// FNV-1a 64-bit mirrors the plugin's `deriveRunId` exactly so tests
  /// can predict the run id and queue a `start` response that validates.
  function predictRunId(callID: string, role: string): string {
    let h = 14695981039346656037n;
    const prime = 1099511628211n;
    const s = `51|implementation|1|${role}|${SESSION_ID}|${callID}`;
    for (let i = 0; i < s.length; i++) {
      h ^= BigInt(s.charCodeAt(i));
      h = (h * prime) & 0xffffffffffffffffn;
    }
    const hex = h.toString(16).padStart(16, "0");
    const id = `pl-51-1-${hex}`;
    return id.length > 128 ? id.slice(0, 128) : id;
  }

  function startResponse(callID: string, role: string): { run_id: string; created: boolean } {
    return { run_id: predictRunId(callID, role), created: true };
  }

  /// Standard scripted queue covering the five calls every successful
  /// lifecycle issues, in execution order. Each test passes its own
  /// `notes` payload for the audit comment and the expected `changed`
  /// payloads for the entry/exit status advances.
  function queue(callID: string, role: string, notes: string, changed = true): unknown[] {
    return [
      ENTRY_OK,
      startResponse(callID, role),
      { notes },
      "",
      { changed },
    ];
  }

  async function runLifecycle(
    callID: string,
    role: string,
    output: string,
    responses: Array<unknown>,
  ): Promise<string[][]> {
    const hooks = await makeHooks();
    setFakeStdout(responses);
    const b = beforeArgs(callID, role);
    await hooks["tool.execute.before"]!(b.input, b.output);
    const a = afterArgs(callID, role, output);
    await hooks["tool.execute.after"]!(a.input, a.output);
    return recordedCalls();
  }

  function timerFinishCalls(calls: string[][]): string[][] {
    return calls.filter((argv) => argv.includes("timer") && argv.includes("finish"));
  }

  function statusAdvanceCalls(calls: string[][]): string[][] {
    return calls.filter((argv) => argv.includes("status") && argv.includes("advance"));
  }

  function advanceTarget(argv: string[]): string {
    return argv[argv.indexOf("--status") + 1];
  }

  function lastFinishResult(calls: string[][]): string {
    const finishes = timerFinishCalls(calls);
    return finishes[finishes.length - 1][finishes[finishes.length - 1].indexOf("--result") + 1];
  }

  function lastAdvanceTarget(calls: string[][]): string | null {
    const advance = statusAdvanceCalls(calls);
    return advance.length === 0 ? null : advanceTarget(advance[advance.length - 1]);
  }

  function resetCallCount(): void {
    writeFileSync(fakeCallCountLog, "0");
  }

  return {
    logs,
    setFakeStdout,
    makeClient,
    makeHooks,
    recordedCalls,
    beforeArgs,
    afterArgs,
    runLifecycle,
    timerFinishCalls,
    statusAdvanceCalls,
    advanceTarget,
    lastFinishResult,
    lastAdvanceTarget,
    predictRunId,
    startResponse,
    queue,
    executorResult,
    reviewerResult,
    prompt,
    resetCallCount,
  };
}