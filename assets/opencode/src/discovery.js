// Lazy worktree discovery and the per-session placement decision.
//
// A session with neither a registered nor an inherited worktree probes the
// checkout's issue binding and the issue's leases, and reuses a path only when
// one is already recorded. Creating a worktree is opt-in (issue 616): with no
// reusable lease the session keeps the current checkout and is pointed at the
// explicit `phasegent worktree acquire --isolate` command instead. A
// Task-spawned child inherits its parent's directory and never acquires a
// lease of its own. Placement is move-only (issue 623): a decided placement
// that cannot move fails closed and the pending tool call is cancelled.

import {
  issueClosedLocally,
  pickActiveWorktreePath,
  readBranchBinding,
  readIssueLeaseHistory,
  readIssueLeases,
} from "./binding.js";
import { isSubagentSession } from "./roles.js";
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
// a host move rejection cancels the call while an ordinary probe failure keeps
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
// the current checkout, so the next tool call must not repeat the same warning.
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

// A Task-spawned child inherits its parent's directory and never acquires a
// lease of its own; a session without a reported parent keeps the registry
// fallback and the reuse probe. Before that probe, the issue's lease history is
// read: a closed issue (its rows carry the "issue closed…" release reason)
// is refused so the lazy path cannot rebuild a worktree that `issue close`
// just converged. `deps` is an internal seam so tests can exercise that order
// without the phasegent CLI.
export async function ensureSessionWorktree(context, sessionId, event, deps) {
  if (!sessionId) return null;
  const registered = sessionWorktrees.get(String(sessionId));
  if (registered) {
    // Move-only placement (issue 623): a required move that is unavailable or
    // fails throws so the host cancels this invocation; the next call retries.
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
  // The probe stays guarded — discovery failures keep the session in the
  // current checkout — while a decided placement rethrows out of the `try` so
  // the host cancels the call instead of running it unplaced (issue 623).
  try {
    const info = await readInfo(context, sessionId);
    if (info && info.parentID) {
      const target = await inheritedWorktree(context, info.parentID, readInfo);
      if (!target) return null;
      if (info.directory === target) {
        // The host reports the child already inside the parent's directory:
        // confirmed placement, nothing to move.
        markPlaced(sessionId);
        return null;
      }
      rememberWorktree(sessionId, target);
      await moveSessionToWorktree(context, sessionId, target, info.directory);
      return target;
    }
    const remembered = await reuseRememberedWorktree(context, sessionId);
    if (remembered) return remembered;
    const discovered = await discover(sessionId, cwd);
    if (discovered) {
      await moveSessionToWorktree(context, sessionId, discovered);
      return discovered;
    }
    if (isSubagentSession(event)) return null; // sub-agents never acquire
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
