// Local, offline probes around the `phasegent` CLI: branch binding, worktree
// acquire, active lease status, and the full lease history. All calls stay
// local: no network, no credentials, no .env copies.
//
// The role a probe needs is scoped to that single call through the
// `PHASEGENT_ROLE` environment entry, so the host process environment is never
// mutated (issue #588 phase 2).

import { phasegentCommand, safeText, warn } from "./runtime.js";

// `worktree acquire` is orchestrator-only; the lease reads are executor-scoped.
const ORCHESTRATOR_ENV = { PHASEGENT_ROLE: "orchestrator" };
const EXECUTOR_ENV = { PHASEGENT_ROLE: "executor" };
const LEASE_PAGE_LIMIT = 100;
const MAX_LEASE_PAGES = 10_000;
const PAGING_FIELDS = ["items", "page", "limit", "total_count", "has_more"];
const LEASE_STATUSES = new Set(["active", "retained", "released"]);

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

export async function acquireWorktree(issueId, sessionId, cwd, options) {
  // Lease-owned operations need an explicit identity (issue 651 P4):
  // the CLI removed its fabricated fallback owner, so an anonymous
  // acquire would hard-error. Refuse upfront with an actionable warning
  // instead of spending a CLI round-trip on a guaranteed failure; the
  // caller falls back to its ownerless path.
  if (!sessionId) {
    warn(
      `phasegent: cannot acquire a worktree for issue ${issueId} without a session identity; ` +
        "the lease must be owned, so pass the host session id or run " +
        `\`phasegent worktree acquire --issue ${issueId} --isolate\` by hand`,
    );
    return null;
  }
  const args = [
    "worktree", "acquire",
    "--issue", String(issueId),
    "--format", "json",
  ];
  // The host strategy's explicit create request passes `isolate`, which
  // forces a fresh directory; a caller that only wants the reuse decision
  // leaves it unset.
  if (options && options.isolate) args.push("--isolate");
  args.push("--session", String(sessionId));
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

function isObject(value) {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function validLease(lease) {
  return (
    isObject(lease) &&
    typeof lease.lease_id === "string" && lease.lease_id.length > 0 &&
    typeof lease.repo_identity === "string" && lease.repo_identity.length > 0 &&
    Number.isSafeInteger(lease.issue) && lease.issue > 0 &&
    typeof lease.session === "string" && lease.session.length > 0 &&
    typeof lease.checkout_path === "string" && lease.checkout_path.length > 0 &&
    typeof lease.worktree_path === "string" && lease.worktree_path.length > 0 &&
    typeof lease.branch === "string" &&
    LEASE_STATUSES.has(lease.status) &&
    Number.isSafeInteger(lease.created_at) &&
    Number.isSafeInteger(lease.heartbeat_at) &&
    (!Object.hasOwn(lease, "release_reason") || typeof lease.release_reason === "string")
  );
}

function validUniqueLeases(leases) {
  if (!Array.isArray(leases)) return false;
  const ids = new Set();
  for (const lease of leases) {
    if (!validLease(lease) || ids.has(lease.lease_id)) return false;
    ids.add(lease.lease_id);
  }
  return true;
}

function isLegacyLeaseResult(parsed) {
  return !PAGING_FIELDS.some((field) => Object.hasOwn(parsed, field));
}

async function readCompleteLeaseCollection(argsForPage, cwd, contextField, expectedIssue, executePage) {
  const runPage = executePage || ((args) => safeText(phasegentCommand(args, cwd, EXECUTOR_ENV)));
  const leases = [];
  const leaseIds = new Set();
  let expectedContext;
  let totalCount;
  let pageCount;

  for (let page = 1; page <= MAX_LEASE_PAGES; page += 1) {
    const args = [...argsForPage(page), "--page", String(page), "--limit", String(LEASE_PAGE_LIMIT)];
    const result = await runPage(args);
    if (!result.ok || !result.value) return null;

    let parsed;
    try {
      parsed = JSON.parse(result.value);
    } catch (_) {
      return null;
    }
    if (!isObject(parsed)) return null;

    if (page === 1 && isLegacyLeaseResult(parsed)) {
      if (
        !Array.isArray(parsed.leases) ||
        !validUniqueLeases(parsed.leases) ||
        (contextField === "issue" && parsed.issue !== expectedIssue) ||
        (contextField === "repo_identity" &&
          (typeof parsed.repo_identity !== "string" || parsed.repo_identity.length === 0))
      ) {
        return null;
      }
      return parsed.leases;
    }

    if (
      parsed.page !== page ||
      parsed.limit !== LEASE_PAGE_LIMIT ||
      !Array.isArray(parsed.items) ||
      parsed.items.length > LEASE_PAGE_LIMIT ||
      typeof parsed.has_more !== "boolean" ||
      !Number.isSafeInteger(parsed.total_count) ||
      parsed.total_count < 0 ||
      parsed.total_count > LEASE_PAGE_LIMIT * MAX_LEASE_PAGES
    ) {
      return null;
    }

    const context = parsed[contextField];
    if (contextField === "issue") {
      if (!Number.isSafeInteger(context) || context !== expectedIssue) return null;
    } else if (typeof context !== "string" || context.length === 0) {
      return null;
    }
    if (page === 1) {
      expectedContext = context;
      totalCount = parsed.total_count;
      pageCount = Math.max(1, Math.ceil(totalCount / LEASE_PAGE_LIMIT));
      if (pageCount > MAX_LEASE_PAGES) return null;
    } else if (context !== expectedContext || parsed.total_count !== totalCount) {
      return null;
    }

    if (page > pageCount) return null;
    const expectedItems = Math.min(LEASE_PAGE_LIMIT, totalCount - leases.length);
    if (parsed.items.length !== expectedItems) return null;
    const expectedMore = leases.length + parsed.items.length < totalCount;
    if (parsed.has_more !== expectedMore) return null;

    for (const lease of parsed.items) {
      if (!validLease(lease) || leaseIds.has(lease.lease_id)) return null;
      leaseIds.add(lease.lease_id);
      leases.push(lease);
    }

    if (!parsed.has_more) {
      return leases.length === totalCount && page === pageCount ? leases : null;
    }
  }

  return null;
}

export async function readIssueLeases(issueId, cwd, executePage) {
  const issue = Number(issueId);
  if (!Number.isSafeInteger(issue) || issue <= 0) return null;
  const leases = await readCompleteLeaseCollection(
    (page) => ["worktree", "status", "--issue", String(issue)],
    cwd,
    "issue",
    issue,
    executePage,
  );
  return leases;
}

// `worktree status` selects active rows only, so a converged issue looks empty
// there: its closed marker is read through `worktree list --no-sync`, whose
// envelope keeps every row (active and terminal) of the resolved repo
// identity. `--no-sync` keeps the probe local and offline: no provider
// resolution, no reconciliation pass.
export async function readIssueLeaseHistory(issueId, cwd, executePage) {
  const issue = Number(issueId);
  if (!Number.isSafeInteger(issue) || issue <= 0) return null;
  const leases = await readCompleteLeaseCollection(
    () => ["worktree", "list", "--no-sync"],
    cwd,
    "repo_identity",
    undefined,
    executePage,
  );
  return leases === null ? null : leases.filter((lease) => lease.issue === issue);
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
