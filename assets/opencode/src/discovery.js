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

import {
  issueClosedLocally,
  pickActiveWorktreePath,
  readBranchBinding,
  readIssueLeaseHistory,
  readIssueLeases,
} from "./binding.js";
import { errorText, locationDirectory, phasegentCallsDisabled, warn } from "./runtime.js";
import {
  inheritedWorktree,
  issueKnownClosed,
  markPlaced,
  moveSessionToWorktree,
  PLACEMENT_ERROR_PREFIX,
  readSessionInfo,
  rememberClosedIssue,
  rememberWorktree,
  reuseRememberedWorktree,
  sessionWorktrees,
  worktreeForSession,
} from "./session.js";

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
    if (path) rememberWorktree(sessionId, path);
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
// `PHASEGENT_WORKTREE_NO_DISCOVER` keeps the adapter inert beyond the in-memory
// registry: no host session lookup, no CLI, no acquire. The rest is guarded —
// an ordinary discovery failure keeps the session in the current checkout — while
// a decided placement rethrows out of the `try` so the prompt fails instead of
// running unplaced. `deps` is an internal seam so tests can exercise that order
// without the phasegent CLI.
export async function ensureSessionWorktree(context, sessionId, deps) {
  if (!sessionId) return null;
  const registered = sessionWorktrees.get(String(sessionId));
  if (registered) {
    // Move-only placement: a move the host cannot perform fails the prompt.
    await moveSessionToWorktree(context, sessionId, registered);
    return registered;
  }
  // `PHASEGENT_WORKTREE_NO_DISCOVER` keeps the adapter inert beyond the
  // in-memory registry: no host session lookup, no CLI, no acquire.
  if (phasegentCallsDisabled()) return await reuseRememberedWorktree(context, sessionId);
  const readInfo = (deps && deps.readSessionInfo) || readSessionInfo;
  const discover = (deps && deps.discover) || discoverWorktreeForSession;
  const readBinding = (deps && deps.readBinding) || readBranchBinding;
  const readLeaseHistory = (deps && deps.readLeaseHistory) || readIssueLeaseHistory;
  const cwd = locationDirectory(context);
  try {
    // A child inherits before the reuse probe: it takes its parent's directory
    // or nothing at all, never the most recently remembered worktree, which
    // could belong to a sibling.
    const info = await readInfo(context, sessionId);
    if (info && info.parentID) {
      return await inheritParentWorktree(context, sessionId, info, readInfo);
    }
    const remembered = await reuseRememberedWorktree(context, sessionId);
    if (remembered) return remembered;
    const discovered = await discover(sessionId, cwd);
    if (discovered) {
      await moveSessionToWorktree(context, sessionId, discovered);
      return discovered;
    }
    const issueId = await readBinding(cwd);
    if (!issueId) return null;
    if (issueKnownClosed(issueId)) return null;
    if (issueClosedLocally(await readLeaseHistory(issueId, cwd))) {
      rememberClosedIssue(issueId);
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

// The parent's registered target is the destination of a move that may still be
// pending at the parent's next step boundary; the directory the host reports for
// the parent is the fallback once that move has landed. A prompt has run no tool
// yet, so the child's move is admitted and the turn continues: it lands before
// the child's first tool call. A parent with no resolvable directory is not an
// error — the child simply stays where it is.
async function inheritParentWorktree(context, sessionId, info, readInfo) {
  const target = await inheritedWorktree(context, info.parentID, readInfo);
  if (!target) return null;
  if (info.directory === target) {
    // The host reports the child already inside the parent's directory:
    // placement settled, nothing to move.
    markPlaced(sessionId);
    return null;
  }
  rememberWorktree(sessionId, target);
  await moveSessionToWorktree(context, sessionId, target, info.directory);
  return target;
}
