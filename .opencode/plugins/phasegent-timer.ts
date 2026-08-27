// OpenCode plugin: automatic phasegent timer and phase status lifecycle.
// Foreground `task` calls delegating to `executor`/`reviewer` with
// explicit REDMINE_ISSUE context advance the Redmine workflow status
// and run a phasegent timer without orchestrator prompt work:
//   before: status -> In Progress (executor) / In Review (reviewer), timer start
//   after:  audit-validated outcome -> timer finish, then mapped status
// The transition graph lives in phasegent (`status advance` preflights
// it); this plugin only names targets. Final `Closed` stays an
// orchestrator post-push gate. explore/general, background, non-Redmine,
// context-less calls are skipped silently. Disable with
// PHASEGENT_AUTO_TIMER=0/false/off.

import type { OpencodeClient } from "@opencode-ai/sdk";
import type { PluginModule } from "@opencode-ai/plugin";

// Local Bun ambient typing so this file does not need a bun-types dependency.
type Pipe = "ignore" | "inherit" | "pipe";
declare const Bun: {
  env: { [k: string]: string | undefined };
  spawn(options: {
    cmd: string[];
    env?: { [k: string]: string | undefined };
    stdin?: Pipe;
    stdout?: Pipe;
    stderr?: Pipe;
  }): { exited: Promise<number>; stdout: ReadableStream<Uint8Array>; stderr: ReadableStream<Uint8Array> };
};
declare const process: { env: { [k: string]: string | undefined } };

const SERVICE = "phasegent-timer";
const ENV_DISABLED = "PHASEGENT_AUTO_TIMER";
const DEFAULT_BIN = "/home/dev/.cargo/bin/phasegent";
const RUN_ID_BOUND = 128;
const PHASE_BOUND = 80;
const ISSUE_BOUND = 16;
const ATTEMPT_BOUND = 4;
const TEXT_BOUND = 480;
const ROLES = new Set(["executor", "reviewer"]);
const FALSY = new Set(["0", "false", "off", "no"]);

type Role = "executor" | "reviewer";
type Result = "DONE" | "PARTIAL" | "BLOCKED" | "FAILED";
type Parsed = { issue: string; phase: string; attempt: string };
type Ctx = Parsed & { runId: string; role: Role };

/// Status required before the child runs.
const ENTRY_STATUS: Record<Role, string> = { executor: "In Progress", reviewer: "In Review" };

/// Status applied after an audit-validated outcome. Reviewer PASS maps to
/// DONE, FAIL/REQUEST_CHANGES to PARTIAL, and every unusable result to
/// FAILED, so this table covers both roles' documented mappings.
const EXIT_STATUS: Record<Role, Record<Result, string>> = {
  executor: { DONE: "In Review", PARTIAL: "In Review", BLOCKED: "Blocked", FAILED: "Blocked" },
  reviewer: { DONE: "Resolved", PARTIAL: "Changes Requested", BLOCKED: "Blocked", FAILED: "Blocked" },
};

const plugin: PluginModule = {
  id: SERVICE,
  async server({ client }) {
    const calls = new Map<string, Ctx>();
    return {
      async "tool.execute.before"(input, output) {
        // Duplicate before hook for the same callID: the first invocation
        // already advanced the status and started the run.
        if (calls.has(input.callID)) return;
        const ctx = extractContext(input.tool, output.args, input.sessionID, input.callID);
        if (!ctx) return;
        if (FALSY.has((Bun.env[ENV_DISABLED] ?? "").trim().toLowerCase())) {
          await log(client, "debug", `automatic timer disabled; skipping ${input.callID}`);
          return;
        }
        // Status preflight runs first: an illegal or failed transition
        // aborts the delegation before any timer exists.
        await applyStatus(client, ctx, ENTRY_STATUS[ctx.role], "entry");
        if (!(await startTimer(client, ctx))) {
          throw new Error(`phasegent timer start failed for ${ctx.runId}`);
        }
        calls.set(input.callID, ctx);
        await log(
          client,
          "info",
          `timer started run_id=${ctx.runId} issue=${ctx.issue} phase=${ctx.phase} role=${ctx.role} attempt=${ctx.attempt}`,
        );
      },      async "tool.execute.after"(input, output) {
        const ctx = calls.get(input.callID);
        if (!ctx) return;
        calls.delete(input.callID);
        let result: Result;
        try {
          result = await deriveOutcome(client, ctx.issue, output.output, ctx.role);
          await finishTimer(ctx.runId, result);
          await log(client, "info", `timer finished run_id=${ctx.runId} result=${result}`);
        } catch (error) {
          await log(client, "error", bound(`timer finish failed for ${ctx.runId}: ${String(error)}`));
          throw error instanceof Error ? error : new Error(String(error));
        }
        await applyStatus(client, ctx, EXIT_STATUS[ctx.role][result], `exit result=${result}`);
      },
      async dispose() {
        // Best-effort. Hard process crashes before the after hook cannot be
        // recovered here; they are reconciled out of band as a Phase 3 task.
        for (const [, ctx] of calls) {
          try {
            await finishTimer(ctx.runId, "FAILED");
          } catch (error) {
            await log(client, "warn", bound(`dispose could not finish ${ctx.runId}: ${String(error)}`));
          }
        }
        calls.clear();
      },
    };
  },
};

export default plugin;

// ---- Context extraction -----------------------------------------------------

function extractContext(tool: string, args: unknown, sessionID: unknown, callID: unknown): Ctx | null {
  if (tool !== "task" || !args || typeof args !== "object") return null;
  const task = args as Record<string, unknown>;
  const role =
    typeof task.subagent_type === "string" && ROLES.has(task.subagent_type)
      ? (task.subagent_type as Role)
      : null;
  if (!role || task.background === true) return null;
  const prompt = pick(task.prompt);
  if (!prompt || !/REDMINE_ISSUE/.test(prompt)) return null;
  const explicit = parseContext(prompt);
  if (!explicit) return null;
  const identity = `${pick(sessionID) ?? "no-session"}|${pick(callID) ?? "no-call"}`;
  return { runId: deriveRunId(explicit, role, identity), ...explicit, role };
}

function parseContext(prompt: string): Parsed | null {
  const m = prompt.match(/PHASEGENT_CONTEXT:\s*issue=(\S+)\s+phase=(\S+)\s+attempt=(\S+)/);
  if (m) return build(m[1], m[2], m[3]);
  const im = prompt.match(/issue\s+#?(\d+)/i);
  const pm = prompt.match(/phase\s+([a-z0-9][a-z0-9_\-.]*)/i);
  const am = prompt.match(/attempt\s+(\d+)/i) ?? prompt.match(/review\s+round\s+(\d+)/i);
  if (!im || !pm || !am) return null;
  return build(im[1], pm[1], am[1]);
}

function build(issue: string, phase: string, attempt: string): Parsed | null {
  const i = sanitizeToken(issue, ISSUE_BOUND);
  const p = sanitizeToken(phase, PHASE_BOUND);
  const a = sanitizeAttempt(attempt);
  return i && p && a ? { issue: i, phase: p, attempt: a } : null;
}

/// Deterministic per-delegation run id. The hash mixes issue/phase/attempt
/// with role plus session/call identity so a duplicate hook for one call
/// reuses the same id while two distinct delegations of the same phase and
/// attempt can never collide on one timer run.
function deriveRunId(c: Parsed, role: Role, identity: string): string {
  // FNV-1a 32-bit so repeated hooks map to the same id without a dependency.
  let h = 0x811c9dc5;
  const s = `${c.issue}|${c.phase}|${c.attempt}|${role}|${identity}`;
  for (let i = 0; i < s.length; i++) {
    h ^= s.charCodeAt(i);
    h = Math.imul(h, 0x01000193);
  }
  const id = `pl-${c.issue}-${c.attempt}-${(h >>> 0).toString(16).padStart(8, "0")}`;
  return id.length > RUN_ID_BOUND ? id.slice(0, RUN_ID_BOUND) : id;
}

// ---- phasegent command wrappers ---------------------------------------------

function binaryPath(): string {
  const configured = Bun.env.PHASEGENT_BIN;
  return configured && configured.trim() !== "" ? configured.trim() : DEFAULT_BIN;
}

function argv(...rest: string[]): string[] {
  return [binaryPath(), "--role", "orchestrator", "--provider", "redmine", ...rest];
}

function startCmd(ctx: Ctx): string[] {
  return argv("timer", "start", ctx.issue, "--phase", ctx.phase, "--agent-role", ctx.role,
    "--attempt", ctx.attempt, "--run-id", ctx.runId);
}

function finishCmd(runId: string, result: Result): string[] {
  return argv("timer", "finish", runId, "--result", result);
}

/// Advance the issue to `target` through phasegent's centralized policy.
/// Same-status calls are an idempotent no-op inside phasegent; a policy
/// violation fails before any write and its stderr already carries
/// current/target/allowed_next/policy/recovery, surfaced here (bounded)
/// so the AI sees the concrete guidance.
async function applyStatus(client: OpencodeClient, ctx: Ctx, target: string, stage: string): Promise<void> {
  const abort = async (detail: string): Promise<never> => {
    const message = bound(
      `phasegent status ${stage} failed issue=${ctx.issue} target='${target}': ${detail}` +
        ` | recovery: phasegent --role orchestrator --provider redmine status next ${ctx.issue}`,
    );
    await log(client, "error", message);
    throw new Error(message);
  };
  let exitCode = 0;
  let stderr = "";
  try {
    ({ exitCode, stderr } = await run(argv("status", "advance", ctx.issue, "--status", target)));
  } catch (error) {
    return abort(`spawn failed: ${String(error)}`);
  }
  if (exitCode !== 0) return abort(`exit=${exitCode} ${stderr}`);
  await log(client, "info", `status ${stage} issue=${ctx.issue} target=${target}`);
}

async function startTimer(client: OpencodeClient, ctx: Ctx): Promise<boolean> {
  const fail = async (message: string): Promise<boolean> => {
    await log(client, "error", bound(message));
    await bestEffortBlocked(ctx.runId);
    return false;
  };
  let exitCode = 0;
  let stderr = "";
  let stdout = "";
  try {
    ({ exitCode, stdout, stderr } = await run(startCmd(ctx)));
  } catch (error) {
    return fail(`timer start spawn failed run_id=${ctx.runId}: ${String(error)}`);
  }
  if (exitCode !== 0) {
    return fail(`timer start exit=${exitCode} run_id=${ctx.runId} stderr=${stderr}`);
  }
  let responseId: string | null = null;
  try {
    responseId = pick((JSON.parse(stdout) as { run_id?: unknown }).run_id);
  } catch {
    responseId = null;
  }
  return responseId === ctx.runId
    ? true
    : fail(`timer start response run_id=${responseId ?? "<missing>"} did not match ${ctx.runId}`);
}

async function finishTimer(runId: string, result: Result): Promise<void> {
  let exitCode = 0;
  let stderr = "";
  try {
    ({ exitCode, stderr } = await run(finishCmd(runId, result)));
  } catch (error) {
    throw new Error(bound(`timer finish spawn failed: ${String(error)}`));
  }
  if (exitCode !== 0) throw new Error(bound(`timer finish exit=${exitCode} stderr=${stderr}`));
}

/// Best-effort cleanup after a failed start: the start failure itself is
/// the error surfaced to the caller.
async function bestEffortBlocked(runId: string): Promise<void> {
  await run(finishCmd(runId, "BLOCKED")).catch(() => undefined);
}

async function run(command: string[]): Promise<{ exitCode: number; stdout: string; stderr: string }> {
  const handle = Bun.spawn({
    cmd: command,
    env: process.env,
    stdin: "ignore",
    stdout: "pipe",
    stderr: "pipe",
  });
  const [exitCode, out, err] = await Promise.all([handle.exited, drain(handle.stdout), drain(handle.stderr)]);
  return { exitCode, stdout: out.trim(), stderr: err.trim() };
}

async function drain(stream: ReadableStream<Uint8Array>): Promise<string> {
  try {
    return await new Response(stream).text();
  } catch {
    return "";
  }
}

// ---- Outcome derivation -----------------------------------------------------
/// Map the child's reported result onto a timer/status outcome, but only
/// after the published audit note itself confirms it. The child JSON alone
/// is never trusted: the fetched note must contain the exact marker plus a
/// status/verdict line agreeing with the JSON.
async function deriveOutcome(
  client: OpencodeClient,
  issue: string,
  output: unknown,
  role: Role,
): Promise<Result> {
  const reject = async (reason: string): Promise<Result> => {
    await log(client, "warn", bound(`child ${role} ${reason}; mapping to FAILED`));
    return "FAILED";
  };
  const read = readTaskResult(output);
  if (read.state !== "ok") return reject(`returned ${read.state}`);
  let parsed: { tracking?: unknown; status?: unknown; verdict?: unknown };
  try {
    parsed = JSON.parse(read.body) as typeof parsed;
  } catch {
    return reject("JSON parse failed");
  }
  const tracking = (parsed.tracking ?? {}) as Record<string, unknown>;
  const commentId = pick(tracking.comment_id);
  const marker = pick(tracking.marker);
  if (tracking.mode !== "REDMINE_ISSUE" || tracking.comment !== "posted" || !commentId || !marker) {
    return reject("tracking invalid");
  }
  const note = await fetchNote(issue, commentId);
  if (!note) return reject(`audit note ${commentId} unreadable`);
  if (!note.includes(marker)) return reject("audit note missing exact marker");
  const claimed = pick(role === "executor" ? parsed.status : parsed.verdict);
  const label = role === "executor" ? "status" : "VERDICT";
  if (!claimed || !hasLabelled(note, label, claimed)) {
    return reject(`audit note has no ${label} line matching '${claimed ?? "<missing>"}'`);
  }
  if (role === "executor") return mapStatus(claimed);
  if (!hasLabelled(note, "OVERALL_CORRECTNESS")) {
    return reject("audit note missing OVERALL_CORRECTNESS line");
  }
  return mapVerdict(claimed);
}

/// `true` when the note carries a line labelled `label` (optionally with
/// list/heading decoration) whose text contains `token` as a whole word.
function hasLabelled(note: string, label: string, token?: string): boolean {
  const head = new RegExp(`^[\\s>*#\\-]*${label}\\b`, "i");
  const word = token ? new RegExp(`(^|[^A-Za-z0-9_])${token}([^A-Za-z0-9_]|$)`) : null;
  return note.split("\n").some((line) => head.test(line) && (!word || word.test(line)));
}

async function fetchNote(issue: string, commentId: string): Promise<string | null> {
  try {
    const { exitCode, stdout } = await run(argv("comment", "get", issue, commentId));
    if (exitCode !== 0) return null;
    const payload = JSON.parse(stdout) as { notes?: unknown; body?: unknown; comments?: unknown };
    const body = pick(payload.notes) ?? pick(payload.body);
    if (body) return body;
    const first = Array.isArray(payload.comments) ? payload.comments[0] : null;
    return pick((first as { body?: unknown } | null)?.body);
  } catch {
    return null;
  }
}

function mapStatus(status: string): Result {
  return status === "DONE" || status === "PARTIAL" || status === "BLOCKED" ? status : "FAILED";
}

function mapVerdict(verdict: string): Result {
  if (verdict === "PASS") return "DONE";
  if (verdict === "FAIL" || verdict === "REQUEST_CHANGES") return "PARTIAL";
  return verdict === "BLOCKED" ? "BLOCKED" : "FAILED";
}

type Read = { state: "ok"; body: string } | { state: "error" | "missing" | "malformed" };

function readTaskResult(output: unknown): Read {
  if (typeof output !== "string" || output.length === 0) return { state: "error" };
  const m = output.match(/<task_result>\s*([\s\S]*?)\s*<\/task_result>/);
  if (!m) return { state: "missing" };
  const body = m[1] ?? "";
  return body.length > 0 ? { state: "ok", body } : { state: "malformed" };
}

// ---- Helpers ----------------------------------------------------------------

function pick(value: unknown): string | null {
  if (typeof value !== "string") return null;
  const trimmed = value.trim();
  return trimmed.length > 0 ? trimmed : null;
}

function sanitizeToken(value: string, limit: number): string | null {
  if (!value) return null;
  const t = value.trim().replace(/[^\w\-.]/g, "_");
  return /^\w[\w\-.]*$/.test(t) ? (t.length > limit ? t.slice(0, limit) : t) : null;
}

function sanitizeAttempt(value: string): string | null {
  const t = value.trim();
  return /^\d{1,8}$/.test(t) ? t.slice(0, ATTEMPT_BOUND) : null;
}

/// Collapse and length-bound any surfaced command output so a thrown
/// error or log line can never carry a full remote response.
function bound(message: string): string {
  const collapsed = message.replace(/\s+/g, " ").trim();
  return collapsed.length > TEXT_BOUND ? `${collapsed.slice(0, TEXT_BOUND - 3)}...` : collapsed;
}

async function log(
  client: OpencodeClient,
  level: "debug" | "info" | "warn" | "error",
  message: string,
): Promise<void> {
  try {
    await client.app.log({ body: { service: SERVICE, level, message } });
  } catch {
    // Logging must never break the wrapper.
  }
}
