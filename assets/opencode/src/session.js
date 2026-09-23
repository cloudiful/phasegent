// Acquired-worktree registry and session placement (issue #440; parent
// inheritance issue #567).
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

import { errorText, locationDirectory, warn } from "./runtime.js";

export const sessionWorktrees = new Map();
let activeWorktree = null;
const moveAttempts = new Set();
const movedSessions = new Set();
const sessionInfo = new Map();
const closedIssues = new Set();

export function sessionPlaced(sessionId) {
  return sessionId !== undefined && sessionId !== null && movedSessions.has(String(sessionId));
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
// boundary (packages/core/src/session/move.ts:114-155). A confirmed placement
// (`sessionPlaced`) is what lets the hook skip path rewriting, while
// `moveAttempts` only deduplicates concurrent calls: a move that failed is
// retried on the next tool call instead of pinning the session to the old
// directory. `currentDirectory` is the session's own directory when the host
// reports one, so a session that already sits in the target is not moved again.
export async function moveSessionToWorktree(context, sessionId, directory, currentDirectory) {
  if (!context) return;
  if (typeof directory !== "string" || directory.length === 0) return;
  if (sessionId === undefined || sessionId === null) return;
  const key = String(sessionId);
  if (movedSessions.has(key) || moveAttempts.has(key)) return;
  moveAttempts.add(key);
  try {
    const current =
      typeof currentDirectory === "string" && currentDirectory.length > 0
        ? currentDirectory
        : locationDirectory(context);
    if (current === directory) {
      movedSessions.add(key); // the session already sits in the worktree
      return;
    }
    const move = context && context.session && context.session.move;
    if (typeof move !== "function") {
      warn("phasegent: host exposes no session.move; redirecting tool arguments only");
      return;
    }
    await move({ sessionID: sessionId, directory });
    movedSessions.add(key);
  } catch (error) {
    warn(
      `phasegent: session move to ${directory} failed; redirecting tool arguments instead (${errorText(error)})`,
    );
  } finally {
    moveAttempts.delete(key);
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
