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
// The directory a session occupied when its registered target was decided.
//
// The host reports where a session *is*, never whether an accepted move has
// landed yet, so the registry keeps this origin to tell the two apart: a
// target whose move is still pending keeps the session at its origin, while a
// session the host reports anywhere else was relocated. It comes from
// authoritative host metadata only, never from the plugin's context or from the
// cached session record.
const placementOrigins = new Map();
// Targets the host moved a session out of.
//
// A relocation the host made is authoritative and stays authoritative for that
// session, so a later lease rediscovery must not hand the same target back.
// The record is session-local and target-specific: another target, a new issue
// or another worktree is never blocked.
const abandonedTargets = new Map();
// The placement attempt a session runs, the target it moves to, and the host
// move it dispatched. A superseded attempt keeps its dispatch promise, so the
// attempt that replaces it waits for the earlier host move instead of
// overtaking it, while the superseded attempt itself skips the host move it
// never dispatched and publishes no placement state.
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

// True when a fresh host read agreed the session sits at its registered
// directory. Only a confirmation ends the tolerance for a pending target.
export function hostConfirmed(sessionId) {
  return sessionId !== undefined && sessionId !== null && confirmedSessions.has(String(sessionId));
}

// The attempt this session runs, or `null` when no attempt is current: none
// was started, or the one that ran was superseded by another target.
function currentAttempt(key) {
  const record = moveAttempts.get(key);
  return record && record.attempt ? record.attempt : null;
}

// Retires the attempt this session runs without dropping the host move it
// already dispatched: the next attempt waits for that move instead of
// overtaking it.
function supersedeAttempt(key) {
  const record = moveAttempts.get(key);
  if (record) record.attempt = null;
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
//
// `origin` is the directory the session occupied when this target was decided.
// It is the evidence that keeps a pending target valid while the host still
// reports the origin, so a later relocation cannot be mistaken for a move that
// has not landed yet.
//
// The origin and the placement markers belong to one target. Registering the
// same target again keeps them, so a repeated admission cannot destroy the
// evidence of a move that is still pending; registering a different target
// drops them, so markers of the previous target never decide the placement of
// the new one.
export function rememberWorktree(sessionId, directory, origin) {
  if (typeof directory !== "string" || directory.length === 0) return;
  if (sessionId === undefined || sessionId === null) return;
  const key = String(sessionId);
  const previous = sessionWorktrees.get(key);
  if (previous !== undefined && previous !== directory) {
    placementOrigins.delete(key);
    placedSessions.delete(key);
    confirmedSessions.delete(key);
    // The attempt in flight belongs to the previous target: it must not
    // suppress the new move and the new target must not join it. Its dispatched
    // host move stays recorded, so the new move waits for it.
    supersedeAttempt(key);
  }
  sessionWorktrees.set(key, directory);
  if (typeof origin === "string" && origin.length > 0) placementOrigins.set(key, origin);
}

// The origin recorded for a session's registered target, or `null` when none
// was proven.
export function placementOrigin(sessionId) {
  if (sessionId === undefined || sessionId === null) return null;
  const known = placementOrigins.get(String(sessionId));
  return known === undefined ? null : known;
}

// Remembers that the host moved this session out of `directory`. The record
// survives the target it belonged to, so a later lease rediscovery cannot
// resurrect a target the session was relocated away from.
export function markTargetAbandoned(sessionId, directory) {
  if (sessionId === undefined || sessionId === null) return;
  if (typeof directory !== "string" || directory.length === 0) return;
  const key = String(sessionId);
  let targets = abandonedTargets.get(key);
  if (!targets) {
    targets = new Set();
    abandonedTargets.set(key, targets);
  }
  targets.add(directory);
}

// True when the host moved this session out of `directory`.
export function targetAbandoned(sessionId, directory) {
  if (sessionId === undefined || sessionId === null) return false;
  if (typeof directory !== "string" || directory.length === 0) return false;
  const targets = abandonedTargets.get(String(sessionId));
  return targets ? targets.has(directory) : false;
}

// The session's own registered worktree, or `null` when it has none.
export function worktreeForSession(sessionId) {
  if (sessionId === undefined || sessionId === null) return null;
  const known = sessionWorktrees.get(String(sessionId));
  return known === undefined ? null : known;
}

export function resetWorktrees() {
  sessionWorktrees.clear();
  placementOrigins.clear();
  abandonedTargets.clear();
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
// with this session: its registered target, the origin that justified it, the
// settled and confirmed markers, an in-flight move, and the cached host record
// that produced the stale answer. The state is cleared together, so no
// obsolete target can outlive the evidence that supported it. The record of a
// target the host moved the session out of survives: it is the evidence that
// keeps a later rediscovery from handing that target back.
export function forgetSessionWorktree(sessionId, _directory) {
  if (sessionId === undefined || sessionId === null) return;
  const key = String(sessionId);
  sessionWorktrees.delete(key);
  placementOrigins.delete(key);
  sessionInfo.delete(key);
  placedSessions.delete(key);
  confirmedSessions.delete(key);
  supersedeAttempt(key);
}

// The parent's directory is the destination a child follows.
//
// The host is the only authority on where the parent runs, so the parent is
// read fresh on every inheritance: the cached session record can name a
// directory the parent was moved out of long ago. A registered parent target is
// then reconciled against that fresh answer:
//
//   * the host reports the target: the parent sits in it, the child follows;
//   * the host reports the recorded origin and the target was never confirmed:
//     the accepted move has not landed yet, so the child follows the pending
//     target rather than the lagging host answer;
//   * the host reports any other directory — a third one, or the origin of an
//     already confirmed target: the parent was relocated, so its stale target
//     is dropped and the child follows the host.
//
// A parent the host cannot describe gives the child nothing at all: no cached
// directory, no issue-lease discovery, and never a stale target.
export async function inheritedWorktree(context, parentId, readDirectory) {
  if (parentId === undefined || parentId === null) return null;
  const key = String(parentId);
  const registered = sessionWorktrees.get(key);
  const read = readDirectory || readAuthoritativeDirectory;
  const directory = await read(context, parentId);
  if (!directory) return null;
  if (!registered) return directory;
  if (directory === registered) {
    markConfirmed(parentId);
    return registered;
  }
  const origin = placementOrigin(parentId);
  if (origin && directory === origin && !hostConfirmed(parentId)) {
    // The parent's accepted move has not landed yet: the child follows the
    // pending target instead of the lagging host answer.
    return registered;
  }
  markTargetAbandoned(parentId, registered);
  forgetSessionWorktree(parentId, registered);
  placementDiagnostic("skip", { session: key, reason: "reconciled-elsewhere" });
  return directory;
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
  const pending = currentAttempt(key);
  if (pending) {
    await pending;
    return;
  }
  const record = moveAttempts.get(key);
  const earlier = record ? record.dispatched : null;
  let attempt = null;
  attempt = (async () => {
    if (earlier) {
      // An earlier host move may still be in flight. The dispatched moves are
      // serialized, so this request cannot overtake it and be overwritten by
      // its completion. The earlier attempt's own failure is its own.
      try {
        await earlier;
      } catch (_) {
        // The superseded attempt reported its own failure.
      }
      // The attempt may have been superseded while it waited: it must neither
      // read for a placement nor move the session.
      if (currentAttempt(key) !== attempt) return null;
    }
    // The source must be the host's fresh, authoritative answer for this very
    // session. A cached value or the plugin's own location could be stale, and
    // guessing could send the turn into a directory that is about to vanish.
    // The read follows the wait above, so the answer reflects the earlier host
    // move and cannot be invalidated by it.
    const located = await readAuthoritativeDirectory(context, sessionId);
    // The read can outlive the decision: the session may have moved on to
    // another target while it was in flight. A superseded attempt must
    // neither move the session nor publish placement state, so it checks that
    // it is still the attempt this session runs.
    if (currentAttempt(key) !== attempt) return null;
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
    // The fresh read is the session's origin: the directory it occupies before
    // this move. Recording it here gives the next prompt the evidence that
    // separates a move which has not landed yet from a session that was
    // relocated elsewhere, and it is written before the move is requested so a
    // rejected move still leaves the origin known.
    if (!placementOrigins.has(key)) placementOrigins.set(key, located);
    const move = context && context.session && context.session.move;
    if (typeof move !== "function") {
      throw new Error(
        `${PLACEMENT_ERROR_PREFIX} unavailable for '${key}': the host exposes no session.move, ` +
          `so '${key}' cannot enter ${directory} and stays in ${located}`,
      );
    }
    placementDiagnostic("request", { session: key, from: located, to: directory });
    try {
      const dispatched = move({ sessionID: sessionId, directory });
      const running = moveAttempts.get(key);
      if (running) running.dispatched = dispatched;
      await dispatched;
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
    // The host may have taken long enough for the attempt to be superseded, so
    // the placement state is published only while the attempt still runs.
    if (currentAttempt(key) === attempt) placedSessions.add(key);
    // The host *accepted* the move; that is not proof it relocated, so this is
    // deliberately not labelled a confirmation.
    placementDiagnostic("accepted", { session: key, from: located, to: directory });
    return directory;
  })();
  moveAttempts.set(key, { target: directory, attempt, dispatched: earlier || null });
  try {
    await attempt;
  } finally {
    const running = moveAttempts.get(key);
    if (running && running.attempt === attempt) moveAttempts.delete(key);
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
