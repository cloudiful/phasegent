// Lazy worktree discovery and the per-session placement decision.
//
// A session with neither a registered nor an inherited worktree probes the
// checkout's issue binding and the issue's leases before it acquires one. A
// Task-spawned child inherits its parent's directory and never acquires a
// lease of its own.

import {
  acquireWorktree,
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
  moveSessionToWorktree,
  readSessionInfo,
  rememberClosedIssue,
  rememberWorktree,
  reuseRememberedWorktree,
  sessionWorktrees,
  worktreeForSession,
} from "./session.js";

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

// A Task-spawned child inherits its parent's directory and never acquires a
// lease of its own; a session without a reported parent keeps the registry
// fallback and the acquire path. Before that acquire, the issue's lease history
// is read: a closed issue (its rows carry the "issue closed…" release reason)
// is refused so the lazy path cannot rebuild a worktree that `issue close`
// just converged. `deps` is an internal seam so tests can exercise that order
// without the phasegent CLI.
export async function ensureSessionWorktree(context, sessionId, event, deps) {
  if (!sessionId) return null;
  const registered = sessionWorktrees.get(String(sessionId));
  if (registered) {
    await moveSessionToWorktree(context, sessionId, registered);
    return registered;
  }
  // `PHASEGENT_WORKTREE_NO_DISCOVER` keeps the adapter inert beyond the
  // in-memory registry: no host session lookup, no CLI, no acquire.
  if (phasegentCallsDisabled()) return await reuseRememberedWorktree(context, sessionId);
  const readInfo = (deps && deps.readSessionInfo) || readSessionInfo;
  const discover = (deps && deps.discover) || discoverWorktreeForSession;
  const acquire = (deps && deps.acquire) || acquireWorktree;
  const readBinding = (deps && deps.readBinding) || readBranchBinding;
  const readLeaseHistory = (deps && deps.readLeaseHistory) || readIssueLeaseHistory;
  const cwd = locationDirectory(context);
  try {
    const info = await readInfo(context, sessionId);
    if (info && info.parentID) {
      const target = await inheritedWorktree(context, info.parentID, readInfo);
      if (!target) return null;
      if (info.directory === target) {
        // The child already runs inside the parent's directory: nothing to move
        // and nothing to rewrite, while a later parent move stays followed.
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
    const acquired = await acquire(issueId, sessionId, cwd);
    if (!acquired || typeof acquired.path !== "string") {
      warn("phasegent: worktree acquire failed; reusing original directory");
      return null;
    }
    rememberWorktree(sessionId, acquired.path);
    await moveSessionToWorktree(context, sessionId, acquired.path);
    return acquired.path;
  } catch (error) {
    warn(`phasegent: worktree discovery failed; reusing original directory (${errorText(error)})`);
    return null;
  }
}
