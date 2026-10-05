// Acquired-worktree registry and session placement (issue #440; parent
// inheritance issue #567; prompt-time placement issue 37).
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
// Every placement is decided once per session from the session prompt hook, so
// no tool call is ever cancelled to move a session and no landing check has to
// re-read the host record.

import { errorText, locationDirectory } from "./runtime.js";

// Every error the placement itself raises starts with this prefix (issue 37):
// `discovery.ensureSessionWorktree` rethrows exactly these past its guarded
// probe, so a move the host cannot perform fails the prompt instead of running
// the turn in the old checkout, while an ordinary discovery failure still
// degrades to the current checkout.
export const PLACEMENT_ERROR_PREFIX = "phasegent: session placement";

export const sessionWorktrees = new Map();
let activeWorktree = null;
const moveAttempts = new Map();
const placedSessions = new Set();
const sessionInfo = new Map();
const closedIssues = new Set();

// `sessionPlaced` reports a *settled* placement: the session already sat in the
// worktree, or its move was admitted and the runner applies it at the next step
// boundary. Either way the decision is made, so later prompts never re-move it.
export function sessionPlaced(sessionId) {
  return sessionId !== undefined && sessionId !== null && placedSessions.has(String(sessionId));
}

// Records a placement the host already confirms (the session's reported
// directory is the worktree) so no move is issued for it.
export function markPlaced(sessionId) {
  if (sessionId === undefined || sessionId === null) return;
  placedSessions.add(String(sessionId));
}

// A refused acquisition is remembered per issue: the session stays in the
// main checkout, and the next prompt must not repeat the lease probe or
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
  placedSessions.clear();
  sessionInfo.clear();
  closedIssues.clear();
  activeWorktree = null;
}

// `context.session.get` is the host's session lookup: `parentID` names the
// session that spawned this one, and `location.directory` is where the session
// runs. The result is cached per session because the prompt hook must not add a
// host call to every turn; a failed lookup stays uncached so a later prompt can
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
// boundary (packages/core/src/session/move.ts, runner/llm.ts). Placement runs
// from the session prompt hook (issue 37), which is admitted before the runner
// reaches any tool call, so the move lands before the first tool call of the
// turn: there is no in-flight call to protect and nothing to cancel. The move
// itself still fails closed — a host that cannot move the session raises a
// prefixed error that fails the prompt, so no turn is ever run unplaced.
// `moveAttempts` joins concurrent prompts onto one placement, and a failure
// clears with the attempt so the next prompt retries. A session that already
// sits in the target directory (`currentDirectory`, else the plugin location)
// is placed without a move, and every later prompt is a no-op.
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
    const current =
      typeof currentDirectory === "string" && currentDirectory.length > 0
        ? currentDirectory
        : locationDirectory(context);
    if (current === directory) {
      placedSessions.add(key); // the session already sits in the worktree
      return directory;
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
          `(${errorText(error)}); the next prompt retries`,
      );
    }
    placedSessions.add(key);
    return directory;
  })();
  moveAttempts.set(key, attempt);
  try {
    await attempt;
  } finally {
    if (moveAttempts.get(key) === attempt) moveAttempts.delete(key);
  }
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
