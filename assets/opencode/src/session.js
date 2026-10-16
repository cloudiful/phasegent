// Acquired-worktree registry and session placement (issue #440; parent
// inheritance issue #567; prompt-time placement issue 37; exact session
// ownership issue 919 P1).
//
// `ctx.session.move` relocates a session, and the host's session info
// (`ctx.session.get`) reports a Task-spawned child's `parentID` plus the
// session's own `location.directory`. The registry is an exact per-session map:
// a session's worktree is the one recorded under its own id, and there is no
// cross-session fallback. An unknown session therefore has no worktree — it can
// never be handed another session's directory — and a child inherits only its
// parent's directory (the parent's registered target, or the directory the host
// reports for the parent), never the most recently remembered worktree.
//
// The host is the only authority on where a session *is*: a fresh own-session
// read decides whether a known placement still holds, and a decided move whose
// source cannot be verified fails closed rather than running the turn in an
// unverified directory. Every placement is decided once per session from the
// session prompt hook, so no tool call is ever cancelled to move a session.

import { errorText, placementDiagnostic } from "./runtime.js";

// Every error the placement itself raises starts with this prefix (issue 37):
// `discovery.ensureSessionWorktree` rethrows exactly these past its guarded
// probe, so a move the host cannot perform fails the prompt instead of running
// the turn in the old checkout, while an ordinary discovery failure still
// degrades to the current checkout.
export const PLACEMENT_ERROR_PREFIX = "phasegent: session placement";

export const sessionWorktrees = new Map();
const moveAttempts = new Map();
const placedSessions = new Set();
// Sessions whose placement the host has actually confirmed at the registered
// directory. `placedSessions` records an *admitted* placement (the runner
// applies it at the next step boundary); `confirmedSessions` records that a
// fresh host read agreed, which is the only state that lets a reconciliation
// treat a differing host answer as an external relocation rather than a move
// that has not landed yet.
const confirmedSessions = new Set();
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

// Records that a fresh host read agreed the session is at its registered
// directory, so a later differing answer is a relocation and not a lagging host.
export function markConfirmed(sessionId) {
  if (sessionId === undefined || sessionId === null) return;
  confirmedSessions.add(String(sessionId));
}

function hostConfirmed(sessionId) {
  return sessionId !== undefined && sessionId !== null && confirmedSessions.has(String(sessionId));
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

// Exact per-session registration. A miss (unknown or absent session) records
// nothing and never borrows another session's directory.
export function rememberWorktree(sessionId, directory) {
  if (typeof directory !== "string" || directory.length === 0) return;
  if (sessionId === undefined || sessionId === null) return;
  sessionWorktrees.set(String(sessionId), directory);
}

// The session's own registered worktree, or `null` when it has none.
export function worktreeForSession(sessionId) {
  if (sessionId === undefined || sessionId === null) return null;
  const known = sessionWorktrees.get(String(sessionId));
  return known === undefined ? null : known;
}

export function resetWorktrees() {
  sessionWorktrees.clear();
  moveAttempts.clear();
  placedSessions.clear();
  confirmedSessions.clear();
  sessionInfo.clear();
  closedIssues.clear();
}

// `context.session.get` is the host's session lookup: `parentID` names the
// session that spawned this one, and `location.directory` is where the session
// runs. The result is cached per session because the prompt hook must not add a
// host call to every turn; a failed lookup stays uncached so a later prompt can
// still inherit.
async function fetchSessionInfo(context, sessionId) {
  const get = context && context.session && context.session.get;
  if (typeof get !== "function") return null;
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
  return info;
}

function hostSessionLookupAvailable(context) {
  const get = context && context.session && context.session.get;
  return typeof get === "function";
}

export async function readSessionInfo(context, sessionId) {
  if (sessionId === undefined || sessionId === null) return null;
  const key = String(sessionId);
  if (sessionInfo.has(key)) return sessionInfo.get(key);
  if (!hostSessionLookupAvailable(context)) {
    sessionInfo.set(key, null);
    return null;
  }
  const info = await fetchSessionInfo(context, sessionId);
  if (info) sessionInfo.set(key, info);
  return info;
}

// Issue 747 P2: the host is the only authority on where a session runs, so a
// reconciliation is never answered from the cache above — the cached record
// exists to keep an ordinary inheritance lookup off the host, and answering
// "is my placement still valid?" from it is exactly the stale answer that would
// send a session back into a worktree `issue close` already removed. A usable
// answer refreshes the cache so the next ordinary lookup is current too.
export async function readAuthoritativeDirectory(context, sessionId) {
  if (sessionId === undefined || sessionId === null) return null;
  const info = await fetchSessionInfo(context, sessionId);
  if (info) sessionInfo.set(String(sessionId), info);
  return info ? info.directory : null;
}

// Drops every cached reference to a worktree the host no longer associates
// with this session: its registered target, the settled and confirmed markers,
// an in-flight move, and the cached host record that produced the stale answer.
export function forgetSessionWorktree(sessionId, _directory) {
  if (sessionId === undefined || sessionId === null) return;
  const key = String(sessionId);
  sessionWorktrees.delete(key);
  sessionInfo.delete(key);
  placedSessions.delete(key);
  confirmedSessions.delete(key);
  moveAttempts.delete(key);
}

// The parent's registered target is the destination of a move that may still be
// pending at the parent's next step boundary; the directory the host reports for
// the parent is the fallback once that move has landed.
//
// Issue 747 P2: a parent whose placement already *settled and was confirmed by
// the host* is the exception. Nothing about the plugin keeps it there — `issue
// close` returns the closing session to the main checkout through the OpenCode
// API before its worktree is removed — so its registered target is stale by
// definition once the host reports somewhere else, and the child inherits where
// the parent actually is rather than the closed directory. A parent whose move
// was only *admitted* (the host has not applied it yet) keeps its registered
// target: a lagging host answer is not an external relocation. A parent with no
// registration falls back to the directory the host reports for it.
export async function inheritedWorktree(context, parentId, readInfo) {
  if (parentId === undefined || parentId === null) return null;
  const registered = sessionWorktrees.get(String(parentId));
  if (registered) {
    if (sessionPlaced(parentId) && hostConfirmed(parentId)) {
      const directory = await readAuthoritativeDirectory(context, parentId);
      if (directory !== registered) {
        forgetSessionWorktree(parentId, registered);
        return directory;
      }
    }
    return registered;
  }
  const read = readInfo || readSessionInfo;
  const info = await read(context, parentId);
  return info ? info.directory : null;
}

// `context.session.move` hands an active runner the placement at its next step
// boundary (packages/core/src/session/move.ts, runner/llm.ts). Placement runs
// from the session prompt hook (issue 37), which is admitted before the runner
// reaches any tool call, so the move lands before the first tool call of the
// turn: there is no in-flight call to protect and nothing to cancel. The move
// itself still fails closed — a host that cannot report the session's current
// directory, or cannot move it, raises a prefixed error that fails the prompt,
// so no turn is ever run unplaced. `moveAttempts` joins concurrent prompts onto
// one placement, and a failure clears with the attempt so the next prompt
// retries.
//
// Issue 919 P1: the *fresh own-session* directory the host reports — not
// `context.location` and not the `currentDirectory` compatibility hint — is what
// decides whether a move is needed. When the host cannot report it, a decided
// placement is never guessed from plugin context.
export async function moveSessionToWorktree(context, sessionId, directory, _currentDirectory) {
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
    // The source must be the host's fresh, authoritative answer for this very
    // session. A cached value or the plugin's own location could be stale, and
    // guessing could send the turn into a directory that is about to vanish.
    const located = await readAuthoritativeDirectory(context, sessionId);
    if (typeof located !== "string" || located.length === 0) {
      placementDiagnostic("unverified", {
        session: key,
        to: directory,
        reason: "no-authoritative-directory",
      });
      throw new Error(
        `${PLACEMENT_ERROR_PREFIX} unverified for '${key}': the host could not report the ` +
          `current directory, so '${key}' cannot be moved to ${directory}`,
      );
    }
    if (located === directory) {
      placedSessions.add(key); // the session already sits in the worktree
      markConfirmed(key);
      placementDiagnostic("suppressed", {
        session: key,
        from: located,
        to: directory,
        reason: "already-in-target",
      });
      return directory;
    }
    const move = context && context.session && context.session.move;
    if (typeof move !== "function") {
      throw new Error(
        `${PLACEMENT_ERROR_PREFIX} unavailable for '${key}': the host exposes no session.move, ` +
          `so '${key}' cannot enter ${directory} and stays in ${located}`,
      );
    }
    placementDiagnostic("request", { session: key, from: located, to: directory });
    try {
      await move({ sessionID: sessionId, directory });
    } catch (error) {
      placementDiagnostic("rejected", {
        session: key,
        to: directory,
        reason: "move-rejected",
      });
      throw new Error(
        `${PLACEMENT_ERROR_PREFIX} failed for '${key}': moving to ${directory} rejected ` +
          `(${errorText(error)}); the next prompt retries`,
      );
    }
    placedSessions.add(key);
    // The host *accepted* the move; that is not proof it relocated, so this is
    // deliberately not labelled a confirmation.
    placementDiagnostic("accepted", { session: key, from: located, to: directory });
    return directory;
  })();
  moveAttempts.set(key, attempt);
  try {
    await attempt;
  } finally {
    if (moveAttempts.get(key) === attempt) moveAttempts.delete(key);
  }
}

// Exact-session reuse: a session's own registered worktree, or nothing. Never a
// cross-session fallback.
export async function reuseRememberedWorktree(context, sessionId) {
  const known = worktreeForSession(sessionId);
  if (!known) return null;
  await moveSessionToWorktree(context, sessionId, known);
  return known;
}
