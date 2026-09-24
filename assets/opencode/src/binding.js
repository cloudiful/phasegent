// Local, offline probes around the `phasegent` CLI: branch binding, worktree
// acquire, active lease status, and the full lease history. All calls stay
// local: no network, no credentials, no .env copies.
//
// The role a probe needs is scoped to that single call through the
// `PHASEGENT_ROLE` environment entry, so the host process environment is never
// mutated (issue #588 phase 2).

import { phasegentCommand, safeText } from "./runtime.js";

// `worktree acquire` is orchestrator-only; the lease reads are executor-scoped.
const ORCHESTRATOR_ENV = { PHASEGENT_ROLE: "orchestrator" };
const EXECUTOR_ENV = { PHASEGENT_ROLE: "executor" };

export async function readBranchBinding(cwd) {
  const result = await safeText(phasegentCommand(["issue", "status"], cwd));
  if (!result.ok || !result.value) return null;
  try {
    const parsed = JSON.parse(result.value);
    if (parsed && typeof parsed.issue_id === "number" && parsed.issue_id > 0) {
      return parsed.issue_id;
    }
  } catch (_) {
  }
  return null;
}

export async function acquireWorktree(issueId, sessionId, cwd) {
  const args = [
    "worktree", "acquire",
    "--issue", String(issueId),
    "--format", "json",
  ];
  if (sessionId) args.push("--session", String(sessionId));
  const result = await safeText(phasegentCommand(args, cwd, ORCHESTRATOR_ENV));
  if (!result.ok || !result.value) return null;
  try {
    const parsed = JSON.parse(result.value);
    if (parsed && typeof parsed.path === "string" && parsed.path.length > 0) {
      return parsed;
    }
  } catch (_) {
  }
  return null;
}

export async function readIssueLeases(issueId, cwd) {
  const args = ["worktree", "status", "--issue", String(issueId)];
  const result = await safeText(phasegentCommand(args, cwd, EXECUTOR_ENV));
  if (!result.ok || !result.value) return null;
  try {
    const parsed = JSON.parse(result.value);
    return parsed && Array.isArray(parsed.leases) ? parsed.leases : null;
  } catch (_) {
    return null;
  }
}

// `worktree status` selects active rows only, so a converged issue looks empty
// there: its closed marker is read through `worktree list --no-sync`, whose
// envelope keeps every row (active and terminal) of the resolved repo
// identity. `--no-sync` keeps the probe local and offline: no provider
// resolution, no reconciliation pass.
export async function readIssueLeaseHistory(issueId, cwd) {
  const args = ["worktree", "list", "--no-sync"];
  const result = await safeText(phasegentCommand(args, cwd, EXECUTOR_ENV));
  if (!result.ok || !result.value) return null;
  try {
    const parsed = JSON.parse(result.value);
    if (!parsed || !Array.isArray(parsed.leases)) return null;
    return parsed.leases.filter((lease) => lease && Number(lease.issue) === Number(issueId));
  } catch (_) {
    return null;
  }
}

// `issue close` and `issue sync` converge an issue's lease rows instead of
// deleting them: the rows keep their status (`retained` / `released`) and carry
// a release reason of "issue closed" ("issue closed: <session>") or "issue
// closed on the remote (issue sync)" (src/lifecycle.rs, src/cli/sync.rs). That
// reason is the adapter's local, offline marker for a closed issue, so the lazy
// path can refuse a fresh worktree without a provider call. The rows come from
// the lease history (`worktree list --no-sync`), which keeps terminal rows.
export const ISSUE_CLOSED_REASON = /^issue closed/i;

export function issueClosedLocally(leases) {
  if (!Array.isArray(leases)) return false;
  return leases.some(
    (lease) =>
      Boolean(lease) &&
      typeof lease === "object" &&
      typeof lease.release_reason === "string" &&
      ISSUE_CLOSED_REASON.test(lease.release_reason.trim()),
  );
}

export function pickActiveWorktreePath(leases) {
  if (!Array.isArray(leases)) return null;
  let best = null;
  for (const lease of leases) {
    if (!lease || typeof lease !== "object") continue;
    if (lease.status !== "active") continue;
    if (typeof lease.worktree_path !== "string" || lease.worktree_path.length === 0) {
      continue;
    }
    if (!best) {
      best = lease;
      continue;
    }
    const heartbeat = lease.heartbeat_at ?? "";
    const bestHeartbeat = best.heartbeat_at ?? "";
    if (String(heartbeat) > String(bestHeartbeat)) {
      best = lease;
    }
  }
  return best ? best.worktree_path : null;
}
