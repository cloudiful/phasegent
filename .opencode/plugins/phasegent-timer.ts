// OpenCode plugin: automatic phasegent timer + status lifecycle.
import type { OpencodeClient } from "@opencode-ai/sdk";
import type { PluginModule } from "@opencode-ai/plugin";
import { accessSync, constants, statSync } from "node:fs";
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
const OWNER_BOUND = 128;
const TEXT_BOUND = 480;
const ROLES = new Set(["executor", "reviewer"]);
const FALSY = new Set(["0", "false", "off", "no"]);
const FINISH_MAX_ATTEMPTS = 3;
const FINISH_RETRY_BASE_DELAY_MS = 250;
type Role = "executor" | "reviewer";
type Result = "DONE" | "PARTIAL" | "BLOCKED" | "FAILED";
type Parsed = { issue: string; phase: string; attempt: string };
type Owner = { sessionId: string; callId: string };
type Ctx = Parsed & { runId: string; role: Role; owner: Owner };
const ENTRY_STATUS: Record<Role, string> = { executor: "In Progress", reviewer: "In Review" };
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
        if (calls.has(input.callID)) return;
        const probe = probeContext(input.tool, output.args, input.sessionID, input.callID);
        if (probe.kind === "background") {
          const recover = "phasegent --role orchestrator --provider redmine timer recover <RUN_ID>";
          await log(client, "warn", bound(`background task skipped; phasegent timer does not track background subagents. issue=${probe.issue ?? "<unknown>"} role=${probe.role ?? "<unknown>"} — rerun foreground or use '${recover}' if a previous run is orphaned`));
          return;
        }
        if (probe.kind === "skip") return;
        const ctx = probe.ctx;
        if (FALSY.has((Bun.env[ENV_DISABLED] ?? "").trim().toLowerCase())) {
          await log(client, "debug", `automatic timer disabled; skipping ${input.callID}`);
          return;
        }
        await applyStatus(client, ctx, ENTRY_STATUS[ctx.role], "entry");
        if (!(await startTimer(client, ctx))) throw new Error(`phasegent timer start failed for ${ctx.runId}`);
        calls.set(input.callID, ctx);
        await log(client, "info", `timer started run_id=${ctx.runId} issue=${ctx.issue} phase=${ctx.phase} role=${ctx.role} attempt=${ctx.attempt} owner_session=${ctx.owner.sessionId} owner_call=${ctx.owner.callId}`);
      },
      async "tool.execute.after"(input, output) {
        const ctx = calls.get(input.callID);
        if (!ctx) return;
        calls.delete(input.callID);
        let result: Result;
        try {
          result = await deriveOutcome(client, ctx.issue, output.output, ctx.role);
          await finishTimerWithRetry(ctx.runId, result);
          await log(client, "info", `timer finished run_id=${ctx.runId} result=${result}`);
        } catch (error) {
          await log(client, "error", bound(`timer finish failed for ${ctx.runId}: ${String(error)}`));
          throw error instanceof Error ? error : new Error(String(error));
        }
        await applyStatus(client, ctx, EXIT_STATUS[ctx.role][result], `exit result=${result}`);
      },
      async dispose() {
        for (const [, ctx] of calls) {
          try {
            await finishTimerWithRetry(ctx.runId, "FAILED");
            await log(client, "info", `dispose finished in-flight run_id=${ctx.runId} as FAILED (child did not claim success)`);
          } catch {
            await log(client, "warn", bound(`dispose could not finish run_id=${ctx.runId}; needs 'phasegent --role orchestrator --provider redmine timer recover ${ctx.runId}'`));
          }
        }
        calls.clear();
      },
    };
  },
};
export default plugin;
type Probe =
  | { kind: "background"; issue: string | null; role: string | null }
  | { kind: "skip" }
  | { kind: "ready"; ctx: Ctx };
function probeContext(tool: string, args: unknown, sessionID: unknown, callID: unknown): Probe {
  if (tool !== "task" || !args || typeof args !== "object") return { kind: "skip" };
  const task = args as Record<string, unknown>;
  const role = typeof task.subagent_type === "string" && ROLES.has(task.subagent_type) ? (task.subagent_type as Role) : null;
  const prompt = pick(task.prompt);
  if (task.background === true) {
    const m = (prompt ?? "").match(/PHASEGENT_CONTEXT:\s*issue=(\d+)/) ?? (prompt ?? "").match(/issue\s+#?(\d+)/i);
    return { kind: "background", issue: m ? m[1] : null, role };
  }
  if (!role || !prompt || !/REDMINE_ISSUE/.test(prompt)) return { kind: "skip" };
  const explicit = parseContext(prompt);
  if (!explicit) return { kind: "skip" };
  const owner = { sessionId: sanitizeOwner(pick(sessionID)), callId: sanitizeOwner(pick(callID)) };
  const ctx: Ctx = { runId: deriveRunId(explicit, role, owner), ...explicit, role, owner };
  return { kind: "ready", ctx };
}
function sanitizeOwner(value: string | null): string {
  if (!value) return "no-owner";
  const cleaned = value.replace(/[^\x20-\x7e]/g, "_").trim();
  return !cleaned ? "no-owner" : cleaned.length > OWNER_BOUND ? cleaned.slice(0, OWNER_BOUND) : cleaned;
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
function deriveRunId(c: Parsed, role: Role, owner: Owner): string {
  let h = 14695981039346656037n;
  const p = 1099511628211n;
  const s = `${c.issue}|${c.phase}|${c.attempt}|${role}|${owner.sessionId}|${owner.callId}`;
  for (let i = 0; i < s.length; i++) { h ^= BigInt(s.charCodeAt(i)); h = (h * p) & 0xffffffffffffffffn; }
  const hex = h.toString(16).padStart(16, "0");
  const id = `pl-${c.issue}-${c.attempt}-${hex}`;
  return id.length > RUN_ID_BOUND ? id.slice(0, RUN_ID_BOUND) : id;
}
function isExecutable(path: string): boolean {
  try { const st = statSync(path); if (!st.isFile()) return false; accessSync(path, constants.X_OK); return true; } catch { return false; }
}
function binaryPath(): string {
  const cands: string[] = [];
  const o = Bun.env.PHASEGENT_BIN?.trim(); if (o) cands.push(o);
  cands.push("/workspace/tools/phasegent/target/debug/phasegent");
  cands.push(DEFAULT_BIN);
  const pe = Bun.env.PATH ?? process.env.PATH ?? "";
  for (const d of pe.split(":")) { if (!d) continue; const p = `${d.replace(/\/$/, "")}/phasegent`; if (!cands.includes(p)) cands.push(p); }
  cands.push("phasegent");
  for (const c of cands) { if (c === "phasegent") return c; if (isExecutable(c)) return c; }
  return DEFAULT_BIN;
}
function autoCandidates(): string[] {
  const list: string[] = [];
  const worktree = "/workspace/tools/phasegent/target/debug/phasegent";
  if (isExecutable(worktree)) list.push(worktree);
  if (isExecutable(DEFAULT_BIN)) list.push(DEFAULT_BIN);
  const pe = Bun.env.PATH ?? process.env.PATH ?? "";
  for (const d of pe.split(":")) { if (!d) continue; const p = `${d.replace(/\/$/, "")}/phasegent`; if (list.includes(p)) continue; if (isExecutable(p)) list.push(p); }
  list.push("phasegent");
  return list;
}
function argv(...rest: string[]): string[] { return [binaryPath(), "--role", "orchestrator", "--provider", "redmine", ...rest]; }
function startCmd(ctx: Ctx): string[] {
  return [...argv("timer", "start", ctx.issue), "--phase", ctx.phase, "--agent-role", ctx.role, "--attempt", ctx.attempt, "--run-id", ctx.runId, "--owner-session-id", ctx.owner.sessionId, "--owner-call-id", ctx.owner.callId];
}
function finishCmd(runId: string, result: Result): string[] { return argv("timer", "finish", runId, "--result", result); }
async function applyStatus(client: OpencodeClient, ctx: Ctx, target: string, stage: string): Promise<void> {
  const abort = async (detail: string): Promise<never> => {
    const message = bound(`phasegent status ${stage} failed issue=${ctx.issue} target='${target}': ${detail} | recovery: phasegent --role orchestrator --provider redmine status next ${ctx.issue}`);
    await log(client, "error", message); throw new Error(message);
  };
  let exitCode = 0; let stderr = "";
  try { ({ exitCode, stderr } = await run(argv("status", "advance", ctx.issue, "--status", target))); } catch (error) { return abort(`spawn failed: ${String(error)}`); }
  if (exitCode !== 0) return abort(`exit=${exitCode} ${stderr}`);
  await log(client, "info", `status ${stage} issue=${ctx.issue} target=${target}`);
}
async function startTimer(client: OpencodeClient, ctx: Ctx): Promise<boolean> {
  const fail = async (message: string): Promise<boolean> => { await log(client, "error", bound(message)); await run(finishCmd(ctx.runId, "BLOCKED")).catch(() => undefined); return false; };
  let result: { exitCode: number; stdout: string; stderr: string };
  try { result = await run(startCmd(ctx)); } catch (error) { return fail(`timer start spawn failed run_id=${ctx.runId}: ${String(error)}`); }
  if (result.exitCode !== 0) return fail(`timer start exit=${result.exitCode} run_id=${ctx.runId} stderr=${result.stderr}`);
  let responseId: string | null = null;
  try { responseId = pick((JSON.parse(result.stdout) as { run_id?: unknown }).run_id); } catch { responseId = null; }
  return responseId === ctx.runId ? true : fail(`timer start response run_id=${responseId ?? "<missing>"} did not match ${ctx.runId}`);
}
async function finishTimerWithRetry(runId: string, result: Result): Promise<void> {
  let lastError: unknown = null;
  for (let attempt = 1; attempt <= FINISH_MAX_ATTEMPTS; attempt++) {
    try {
      const outcome = await run(finishCmd(runId, result));
      if (outcome.exitCode !== 0) throw new Error(bound(`timer finish exit=${outcome.exitCode} stderr=${outcome.stderr}`));
      return;
    } catch (error) {
      lastError = error;
      const message = error instanceof Error ? error.message : String(error);
      const isStructured = message.includes('{"error"') || /kind":"(config|not_supported|auth|permission)/.test(message);
      if (isStructured || attempt === FINISH_MAX_ATTEMPTS) break;
      await new Promise<void>((resolve) => setTimeout(resolve, FINISH_RETRY_BASE_DELAY_MS * attempt));
    }
  }
  throw lastError instanceof Error ? lastError : new Error(bound(`timer finish retries exhausted: ${String(lastError)}`));
}
async function run(command: string[]): Promise<{ exitCode: number; stdout: string; stderr: string }> {
  const args = command.slice(1);
  const explicit = Bun.env.PHASEGENT_BIN?.trim();
  const hasExplicit = !!explicit && isExecutable(explicit);
  let candidates: string[];
  if (hasExplicit) candidates = [explicit!];
  else candidates = autoCandidates();
  let last: { exitCode: number; stdout: string; stderr: string } | null = null;
  let lastSpawnError: unknown = null;
  for (const bin of candidates) {
    try {
      const handle = Bun.spawn({ cmd: [bin, ...args], env: process.env, stdin: "ignore", stdout: "pipe", stderr: "pipe" });
      const [exitCode, out, err] = await Promise.all([handle.exited, new Response(handle.stdout).text().catch(() => ""), new Response(handle.stderr).text().catch(() => "")]);
      const stdout = out.trim(); const stderr = err.trim();
      if (exitCode === 0) return { exitCode, stdout, stderr };
      const lower = stderr.toLowerCase();
      const isCompat = lower.includes("unknown option") || lower.includes("unrecognized") || lower.includes("incompatible") || (lower.includes("owner") && lower.includes("unknown"));
      if (isCompat) { last = { exitCode, stdout, stderr }; continue; }
      return { exitCode, stdout, stderr };
    } catch (e) { lastSpawnError = e; continue; }
  }
  if (last) return last;
  if (lastSpawnError) throw lastSpawnError instanceof Error ? lastSpawnError : new Error(String(lastSpawnError));
  throw new Error(bound(`no phasegent binary available; tried ${candidates.join(", ")}`));
}
async function deriveOutcome(client: OpencodeClient, issue: string, output: unknown, role: Role): Promise<Result> {
  const reject = (reason: string): Result => { void log(client, "warn", bound(`child ${role} ${reason}; mapping to FAILED`)); return "FAILED"; };
  const read = readTaskResult(output);
  if (read.state !== "ok") return reject(`returned ${read.state}`);
  let parsed: { tracking?: unknown; status?: unknown; verdict?: unknown };
  try { parsed = JSON.parse(read.body) as typeof parsed; } catch { return reject("JSON parse failed"); }
  const tracking = (parsed.tracking ?? {}) as Record<string, unknown>;
  const commentId = pick(tracking.comment_id); const marker = pick(tracking.marker);
  if (tracking.mode !== "REDMINE_ISSUE" || tracking.comment !== "posted" || !commentId || !marker) return reject("tracking invalid");
  const note = await fetchNote(issue, commentId);
  if (!note) return reject(`audit note ${commentId} unreadable`);
  if (!note.includes(marker)) return reject("audit note missing exact marker");
  const claimed = pick(role === "executor" ? parsed.status : parsed.verdict);
  const label = role === "executor" ? "status" : "VERDICT";
  if (!claimed || !hasLabelled(note, label, claimed)) return reject(`audit note has no ${label} line matching '${claimed ?? "<missing>"}'`);
  if (role === "executor") return mapExecutorStatus(claimed);
  if (!hasLabelled(note, "OVERALL_CORRECTNESS")) return reject("audit note missing OVERALL_CORRECTNESS line");
  return mapReviewerVerdict(claimed);
}
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
  } catch { return null; }
}
function mapExecutorStatus(token: string): Result {
  return token === "DONE" || token === "PARTIAL" || token === "BLOCKED" || token === "FAILED" ? (token as Result) : "FAILED";
}
function mapReviewerVerdict(token: string): Result {
  if (token === "PASS") return "DONE";
  if (token === "FAIL" || token === "REQUEST_CHANGES") return "PARTIAL";
  if (token === "BLOCKED") return "BLOCKED";
  if (token === "AUDIT_FAILED") return "FAILED";
  return "FAILED";
}
function readTaskResult(output: unknown): { state: "ok"; body: string } | { state: "error" | "missing" | "malformed" } {
  if (typeof output !== "string" || output.length === 0) return { state: "error" };
  const m = output.match(/<task_result>\s*([\s\S]*?)\s*<\/task_result>/);
  if (!m) return { state: "missing" };
  const body = m[1] ?? "";
  return body.length > 0 ? { state: "ok", body } : { state: "malformed" };
}
function pick(value: unknown): string | null { if (typeof value !== "string") return null; const trimmed = value.trim(); return trimmed.length > 0 ? trimmed : null; }
function sanitizeToken(value: string, limit: number): string | null { if (!value) return null; const t = value.trim().replace(/[^\w\-.]/g, "_"); return /^\w[\w\-.]*$/.test(t) ? (t.length > limit ? t.slice(0, limit) : t) : null; }
function sanitizeAttempt(value: string): string | null { const t = value.trim(); return /^\d{1,8}$/.test(t) ? t.slice(0, ATTEMPT_BOUND) : null; }
function bound(message: string): string { const collapsed = message.replace(/\s+/g, " ").trim(); return collapsed.length > TEXT_BOUND ? `${collapsed.slice(0, TEXT_BOUND - 3)}...` : collapsed; }
async function log(client: OpencodeClient, level: "debug" | "info" | "warn" | "error", message: string): Promise<void> { try { await client.app.log({ body: { service: SERVICE, level, message } }); } catch { /* logging must never break the wrapper */ } }
export const __private = { deriveRunId, isExecutable, binaryPath, autoCandidates };
