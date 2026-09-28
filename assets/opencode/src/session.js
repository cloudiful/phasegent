// Acquired-worktree registry and session placement (issue #440; parent
// inheritance issue #567; move-only fail-closed placement issue 623).
//
// `ctx.session.move` relocates a session, and the host's session info
// (`ctx.session.get`) reports a Task-spawned child's `parentID` plus the
// session's own `location.directory`. A child therefore inherits its parent's
// directory — the parent's registered target first, then the directory the host
// reports for the parent — instead of guessing from the most recently
// remembered worktree. The registry keeps the per-session mapping, and
// `activeWorktree` stays the fallback for sessions whose parentage the host
// cannot report. An empty registry still tries parent inheritance, so only a
// session with neither a recorded nor an inherited worktree stays untouched.

import { errorText, locationDirectory } from "./runtime.js";

// Every error the fail-closed placement itself raises starts with this prefix
// (issue 623): `discovery.ensureSessionWorktree` rethrows exactly these past
// its guarded probe, so a move rejection cancels the pending tool call while
// an ordinary discovery failure still degrades to the current checkout.
export const PLACEMENT_ERROR_PREFIX = "phasegent: session placement";

export const sessionWorktrees = new Map();
let activeWorktree = null;
const moveAttempts = new Map();
const movedSessions = new Set();
const placedSessions = new Set();
const sessionInfo = new Map();
const closedIssues = new Set();

// `sessionPlaced` reports a *confirmed* placement: the host itself says the
// session sits in the worktree (it started there, or the admitted move landed).
export function sessionPlaced(sessionId) {
  return sessionId !== undefined && sessionId !== null && placedSessions.has(String(sessionId));
}

// Records a host-confirmed placement (the session's reported directory is the
// worktree) so the call proceeds without a move and later calls skip it.
export function markPlaced(sessionId) {
  if (sessionId === undefined || sessionId === null) return;
  const key = String(sessionId);
  placedSessions.add(key);
  movedSessions.add(key);
}

// A refused acquisition is remembered per issue: the session stays in the
// main checkout, and the next tool call must not repeat the lease probe or
// the warning.
export function issueKnownClosed(issueId) {
  return closedIssues.has(String(issueId));
}

export function rememberClosedIssue(issueId) {
  closedIssues.add(String(issueId));
}

export function rememberWorktree(sessionId, directory) {
  if (typeof directory !== "string" || directory.length === 0) return;
  activeWorktree = directory;
  if (sessionId !== undefined && sessionId !== null) {
    sessionWorktrees.set(String(sessionId), directory);
  }
}

export function worktreeForSession(sessionId) {
  if (sessionId !== undefined && sessionId !== null) {
    const known = sessionWorktrees.get(String(sessionId));
    if (known) return known;
  }
  return activeWorktree;
}

export function resetWorktrees() {
  sessionWorktrees.clear();
  moveAttempts.clear();
  movedSessions.clear();
  placedSessions.clear();
  sessionInfo.clear();
  closedIssues.clear();
  activeWorktree = null;
}

// `context.session.get` is the host's session lookup: `parentID` names the
// session that spawned this one, and `location.directory` is where the session
// runs. The result is cached per session because the hook must not add a host
// call to every tool call; a failed lookup stays uncached so a later call can
// still inherit.
export async function readSessionInfo(context, sessionId) {
  if (sessionId === undefined || sessionId === null) return null;
  const key = String(sessionId);
  if (sessionInfo.has(key)) return sessionInfo.get(key);
  const get = context && context.session && context.session.get;
  if (typeof get !== "function") {
    sessionInfo.set(key, null);
    return null;
  }
  let info = null;
  try {
    const result = await get({ sessionID: sessionId });
    if (result && typeof result === "object") {
      const parentID = typeof result.parentID === "string" ? result.parentID : "";
      const directory =
        result.location && typeof result.location.directory === "string"
          ? result.location.directory
          : "";
      info = { parentID: parentID || null, directory: directory || null };
    }
  } catch (_) {
    info = null;
  }
  if (info) sessionInfo.set(key, info);
  return info;
}

// The parent's registered target is the destination of a move that may still be
// pending at the parent's next step boundary; the directory the host reports for
// the parent is the fallback once that move has landed.
export async function inheritedWorktree(context, parentId, readInfo) {
  if (parentId === undefined || parentId === null) return null;
  const registered = sessionWorktrees.get(String(parentId));
  if (registered) return registered;
  const read = readInfo || readSessionInfo;
  const info = await read(context, parentId);
  return info ? info.directory : null;
}

// `context.session.move` hands an active runner the placement at its next step
// boundary (packages/core/src/session/move.ts, runner/llm.ts), so admitting a
// move never makes the initiating tool call safe — it is still running in the
// old directory. Every call that finds the placement not yet effective fails
// closed (issue 623): the error carries `PLACEMENT_ERROR_PREFIX`, reaches the
// `tool.execute.before` hook, and OpenCode cancels that invocation; the next
// invocation retries and, once the host confirms the session sits in the
// target, runs placed. `moveAttempts` joins concurrent callers onto one
// placement so they inherit the same outcome, and a failure clears with the
// attempt so the next call retries. A session that already sits in the target
// directory (`currentDirectory`, else the plugin location) is placed without a
// move, and a second call after a successful placement is a no-op.
export async function moveSessionToWorktree(context, sessionId, directory, currentDirectory) {
  if (typeof directory !== "string" || directory.length === 0) return;
  if (sessionId === undefined || sessionId === null) return;
  const key = String(sessionId);
  if (placedSessions.has(key)) return;
  if (!context) {
    throw new Error(
      `${PLACEMENT_ERROR_PREFIX} unavailable for '${key}': no host context, so '${key}' ` +
        `cannot enter ${directory}`,
    );
  }
  const pending = moveAttempts.get(key);
  if (pending) {
    await pending;
    return;
  }
  const attempt = (async () => {
    if (movedSessions.has(key)) {
      // A previous call already admitted the move. The host's own session
      // record is the only proof the placement has landed — the runner applies
      // it at a step boundary, so a cached discovery snapshot still shows the
      // old directory.
      const landed = await hostSessionDirectory(context, sessionId);
      if (landed !== directory) {
        throw new Error(
          `${PLACEMENT_ERROR_PREFIX} pending for '${key}': the move to ${directory} has not ` +
            "landed yet; this invocation is cancelled so it does not run in the old " +
            "directory, and the next invocation retries",
        );
      }
      placedSessions.add(key);
      return;
    }
    const current =
      typeof currentDirectory === "string" && currentDirectory.length > 0
        ? currentDirectory
        : locationDirectory(context);
    if (current === directory) {
      movedSessions.add(key); // the session already sits in the worktree
      placedSessions.add(key);
      return;
    }
    const move = context && context.session && context.session.move;
    if (typeof move !== "function") {
      throw new Error(
        `${PLACEMENT_ERROR_PREFIX} unavailable for '${key}': the host exposes no session.move, ` +
          `so '${key}' cannot enter ${directory} and stays in ${current}`,
      );
    }
    try {
      await move({ sessionID: sessionId, directory });
    } catch (error) {
      throw new Error(
        `${PLACEMENT_ERROR_PREFIX} failed for '${key}': moving to ${directory} rejected ` +
          `(${errorText(error)}); the next invocation retries`,
      );
    }
    movedSessions.add(key);
    // The move is admitted but lands after this tool call ends, so the
    // initiating call is cancelled and the model's retry runs placed.
    throw new Error(
      `${PLACEMENT_ERROR_PREFIX} pending for '${key}': the move to ${directory} is admitted ` +
        "and lands at the runner's next step boundary; this invocation is cancelled so " +
        "it does not run in the old directory, and the retry runs placed",
    );
  })();
  moveAttempts.set(key, attempt);
  try {
    await attempt;
  } finally {
    if (moveAttempts.get(key) === attempt) moveAttempts.delete(key);
  }
}

// The un-cached host lookup that confirms a landing move: the placement check
// must see the current record, not the pre-move snapshot `readSessionInfo`
// may have cached.
async function hostSessionDirectory(context, sessionId) {
  const get = context && context.session && context.session.get;
  if (typeof get !== "function") return null;
  try {
    const result = await get({ sessionID: sessionId });
    if (result && typeof result === "object" && result.location) {
      const directory = result.location.directory;
      return typeof directory === "string" && directory.length > 0 ? directory : null;
    }
  } catch (_) {
    return null;
  }
  return null;
}

// The registry fallback: a sub-agent whose session id was never remembered
// reuses the most recently remembered worktree. An empty registry means "no
// worktree" and leaves the call untouched.
export async function reuseRememberedWorktree(context, sessionId) {
  const known = worktreeForSession(sessionId);
  if (!known) return null;
  await moveSessionToWorktree(context, sessionId, known);
  return known;
}
