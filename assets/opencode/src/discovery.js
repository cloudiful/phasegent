// Lazy worktree discovery and the per-session placement decision.
//
// Placement runs from the session prompt hook (issue 37), once per session and
// before the runner reaches any tool call, so a move never has to cancel an
// in-flight invocation. A session with neither a registered nor an inherited
// worktree probes the checkout's issue binding and the issue's leases, and reuses
// a path only when one is already recorded. Creating a worktree is opt-in
// (issue 616): with no reusable lease the session keeps the current checkout and
// is pointed at the explicit `phasegent worktree acquire --isolate` command
// instead. A Task-spawned child follows its parent's directory first and never
// reaches the probes below, so it can neither acquire nor guess one.
//
// Issue 919 P1: the registry is exact per session. There is no cross-session
// fallback, so an unknown session — or a child whose parent cannot be resolved —
// stays where it is instead of being redirected into another session's worktree.
//
// The host is the authority for both an own registered target and an inherited
// one, and it is read fresh for every placement decision. A cached session
// record or the plugin's own context is never placement evidence, so a session
// the host relocated out of band is never moved back to a stale directory, and
// a target it was moved out of is not handed back by a later rediscovery.

import {
  issueClosedLocally,
  pickActiveWorktreePath,
  readBranchBinding,
  readIssueLeaseHistory,
  readIssueLeases,
} from "./binding.js";
import { errorText, phasegentCallsDisabled, warn } from "./runtime.js";
import {
  forgetSessionWorktree,
  inheritedWorktree,
  issueKnownClosed,
  markConfirmed,
  markPlaced,
  moveSessionToWorktree,
  PLACEMENT_ERROR_PREFIX,
  readAuthoritativeDirectory,
  readSessionInfo,
  rememberClosedIssue,
  rememberWorktree,
  targetAbandoned,
  worktreeForSession,
} from "./session.js";
import { reconcileSessionLocation } from "./session-reconcile.js";

// A decided placement must fail closed: only errors raised by the move itself
// (identified by `PLACEMENT_ERROR_PREFIX`) rethrow past the discovery guard, so
// a host move rejection fails the prompt while an ordinary probe failure keeps
// the session in the current checkout.
function isPlacementFailure(error) {
  return error instanceof Error && error.message.startsWith(PLACEMENT_ERROR_PREFIX);
}

export async function discoverWorktreeForSession(sessionId, cwd) {
  if (phasegentCallsDisabled()) return null;
  try {
    if (!sessionId) return null;
    const known = worktreeForSession(sessionId);
    if (known) return known;
    const issueId = await readBranchBinding(cwd);
    if (!issueId) return null;
    const leases = await readIssueLeases(issueId, cwd);
    if (!leases) return null;
    const path = pickActiveWorktreePath(leases);
    if (!path) return null;
    // A target the host moved this session out of stays abandoned: the
    // relocation is authoritative and rediscovery must not resurrect it. The
    // origin of the target is recorded from authoritative host metadata by the
    // move that follows this registration, never from the plugin's context.
    if (targetAbandoned(sessionId, path)) return null;
    rememberWorktree(sessionId, path);
    return path;
  } catch (_) {
    return null;
  }
}

// A refused placement is remembered per (session, issue): the session stays in
// the current checkout, so the next prompt must not repeat the same warning.
// A fresh session id (or a new issue binding) warns again, and a lease that
// appears later is still picked up by the reuse probe before this guard.
const refusedPlacements = new Set();

function warnRefusedPlacement(sessionId, issueId) {
  const key = `${sessionId}:${issueId}`;
  if (refusedPlacements.has(key)) return;
  refusedPlacements.add(key);
  warn(
    `phasegent: no worktree was created for issue ${issueId} (isolation is opt-in); ` +
      "staying in the current checkout. Run `phasegent worktree acquire " +
      `--issue ${issueId} --isolate\` for a dedicated worktree.`,
  );
}

// The one placement decision for a session, run from the session prompt hook
// (issue 37): a Task-spawned child follows its parent's directory first, so it
// never reaches the probes below and can neither acquire nor guess a worktree.
// `PHASEGENT_WORKTREE_NO_DISCOVER` keeps the adapter inert beyond the exact
// in-memory registry: no host session lookup, no CLI, no acquire, and no
// auto-move (the current location cannot be verified without a host read). The
// rest is guarded — an ordinary discovery failure keeps the session in the
// current checkout — while a decided placement rethrows out of the `try` so the
// prompt fails instead of running unplaced. `deps` is an internal seam so tests
// can exercise that order without the phasegent CLI.
export async function ensureSessionWorktree(context, sessionId, deps) {
  if (!sessionId) return null;
  // `PHASEGENT_WORKTREE_NO_DISCOVER` keeps the adapter inert: the exact
  // in-memory lookup is local, and no host read or auto-move follows,
  // because a move whose current location cannot be verified must never run.
  if (phasegentCallsDisabled()) return worktreeForSession(sessionId);
  // The host, not this registry, decides where a session runs. A settled *and*
  // an unsettled placement are reconciled against a fresh host read before its
  // cached target is trusted, so a relocation the host made is never reversed
  // and a worktree that was already removed is never re-entered. An unsettled
  // session is untouched otherwise, so the ordinary first placement still
  // happens.
  const readDirectory =
    (deps && deps.readAuthoritativeDirectory) || readAuthoritativeDirectory;
  if (!(await reconcileSessionLocation(context, sessionId, readDirectory))) return null;
  const readInfo = (deps && deps.readSessionInfo) || readSessionInfo;
  const discover = (deps && deps.discover) || discoverWorktreeForSession;
  const readBinding = (deps && deps.readBinding) || readBranchBinding;
  const readLeaseHistory = (deps && deps.readLeaseHistory) || readIssueLeaseHistory;
  try {
    // A child inherits before its own registration and before the reuse probe:
    // it takes its parent's current directory or nothing at all, never another
    // session's worktree. The parent's fresh location is the authority, so a
    // child whose parent moved follows it.
    const info = await readInfo(context, sessionId);
    if (info && info.parentID) {
      return await inheritParentWorktree(context, sessionId, info, readDirectory);
    }
    const registered = worktreeForSession(sessionId);
    if (registered) {
      // Move-only placement: a move the host cannot perform fails the prompt.
      await moveSessionToWorktree(context, sessionId, registered);
      return registered;
    }
    // The probes run where the session actually is: the plugin's own context
    // location and the cached session record are not the session's directory and
    // never decide where a session is placed.
    const cwd = await readDirectory(context, sessionId);
    const discovered = await discover(sessionId, cwd);
    if (discovered) {
      if (targetAbandoned(sessionId, discovered)) {
        // The host moved this session out of that target, so the rediscovery
        // must not put it back.
        forgetSessionWorktree(sessionId, discovered);
        return null;
      }
      await moveSessionToWorktree(context, sessionId, discovered);
      return discovered;
    }
    const issueId = await readBinding(cwd);
    if (!issueId) return null;
    if (issueKnownClosed(issueId)) return null;
    if (await issueConverged(issueId, cwd, readLeaseHistory)) {
      warn(`phasegent: issue ${issueId} is closed; refusing to acquire a worktree (staying put)`);
      return null;
    }
    // Automatic creation is opt-in (issue 616): with no reusable lease the
    // session keeps the current checkout instead of being moved into a freshly
    // created worktree, and the explicit isolation command is the opt-in.
    warnRefusedPlacement(sessionId, issueId);
    return null;
  } catch (error) {
    if (isPlacementFailure(error)) throw error;
    warn(`phasegent: worktree discovery failed; reusing original directory (${errorText(error)})`);
    return null;
  }
}

// The local, offline closed marker: `issue close` and `issue sync` converge a
// lease row instead of deleting it, so its "issue closed…" release reason is
// what the adapter reads. Remembering the answer keeps the next prompt off the
// CLI. A binding that cannot be read leaves the decision to the caller.
async function issueConverged(issueId, cwd, readLeaseHistory) {
  if (issueKnownClosed(issueId)) return true;
  if (!issueClosedLocally(await readLeaseHistory(issueId, cwd))) return false;
  rememberClosedIssue(issueId);
  return true;
}

// The parent's registered target is the destination of a move that may still be
// pending at the parent's next step boundary; the directory the host reports for
// the parent is the fallback once that move has landed. A prompt has run no tool
// yet, so the child's move is admitted and the turn continues: it lands before
// the child's first tool call. A parent with no resolvable directory is not an
// error — the child simply stays where it is.
//
// The parent's location comes from the host, never from the cached session
// record, and the parent's own stale target is reconciled before the child
// follows it. The child's target is registered here and its origin comes from
// the authoritative read inside the move below.
async function inheritParentWorktree(context, sessionId, info, readDirectory) {
  const target = await inheritedWorktree(context, info.parentID, readDirectory);
  if (!target) return null;
  // The host moved this child out of that target before: the inherited target
  // must not put the child back into it.
  if (targetAbandoned(sessionId, target)) return null;
  if (info.directory === target) {
    // The cached record suggests the child already sits in the parent's
    // directory. That is a placement decision, so the host confirms it with a
    // fresh read before the child is treated as placed.
    const located = await readDirectory(context, sessionId);
    if (located === target) {
      // The child already occupies the parent's directory. The target and its
      // origin are recorded anyway, so a later external relocation of the child
      // can be reconciled, and the child is placed without being moved.
      rememberWorktree(sessionId, target, located);
      markPlaced(sessionId);
      markConfirmed(sessionId);
      return null;
    }
  }
  rememberWorktree(sessionId, target);
  await moveSessionToWorktree(context, sessionId, target, info.directory);
  return target;
}
