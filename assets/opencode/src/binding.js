// Local, offline probes around the `phasegent` CLI: branch binding, worktree
// acquire, and the registered-directory reads. All calls stay local: no
// network, no credentials, no .env copies.
//
// The role a probe needs is scoped to that single call through the
// `PHASEGENT_ROLE` environment entry, so the host process environment is never
// mutated (issue #588 phase 2).

import { phasegentCommand, safeText, warn } from "./runtime.js";

// `worktree acquire` is orchestrator-only; the directory reads are
// executor-scoped.
const ORCHESTRATOR_ENV = { PHASEGENT_ROLE: "orchestrator" };
const EXECUTOR_ENV = { PHASEGENT_ROLE: "executor" };
const DIRECTORY_PAGE_LIMIT = 100;
const MAX_DIRECTORY_PAGES = 10_000;
const PAGING_FIELDS = ["items", "page", "limit", "total_count", "has_more"];

export async function readBranchBinding(cwd) {
  const result = await safeText(phasegentCommand(["issue", "status"], cwd));
  if (!result.ok || !result.value) return null;
  try {
    const parsed = JSON.parse(result.value);
    if (!parsed || !Number.isSafeInteger(parsed.issue_id) || parsed.issue_id <= 0) {
      return null;
    }
    // Placement needs the complete immutable durable task identity of a linked
    // issue in this checkout: task id, instance, provider, project and external
    // id. A branch-name fallback, an unknown legacy scope or a missing task
    // identity is not enough to select another task's directory.
    const linked = Array.isArray(parsed.linked_issues)
      ? parsed.linked_issues.find(
          (entry) =>
            entry &&
            entry.issue_number === parsed.issue_id &&
            Number.isSafeInteger(entry.task_id) &&
            entry.task_id > 0 &&
            nonEmptyString(entry.instance) &&
            nonEmptyString(entry.provider) &&
            nonEmptyString(entry.project) &&
            nonEmptyString(entry.external_id),
        )
      : undefined;
    if (!linked) return null;
    return {
      issueId: parsed.issue_id,
      provider: linked.provider,
      project: linked.project,
      externalId: linked.external_id,
      taskId: linked.task_id,
      instance: linked.instance,
    };
  } catch (_) {
  }
  return null;
}

export async function acquireWorktree(issueId, sessionId, cwd, options) {
  // Acquire needs an explicit identity: the CLI removed its fabricated
  // fallback owner, so an anonymous acquire would hard-error. Refuse upfront
  // with an actionable warning instead of spending a CLI round-trip on a
  // guaranteed failure; the caller falls back to its ownerless path.
  if (!sessionId) {
    warn(
      `phasegent: cannot acquire a worktree for issue ${issueId} without a session identity; ` +
        "the directory association must be owned, so pass the host session id or run " +
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

function nonEmptyString(value) {
  return typeof value === "string" && value.length > 0;
}

function validTask(task) {
  return (
    isObject(task) &&
    Number.isSafeInteger(task.task_id) && task.task_id > 0 &&
    Number.isSafeInteger(task.issue) && task.issue > 0 &&
    (!Object.hasOwn(task, "provider") || typeof task.provider === "string") &&
    (!Object.hasOwn(task, "instance") || typeof task.instance === "string") &&
    (!Object.hasOwn(task, "project") || typeof task.project === "string") &&
    (!Object.hasOwn(task, "external_id") || typeof task.external_id === "string")
  );
}

function validLegacyIssue(issue) {
  return (
    isObject(issue) &&
    Number.isSafeInteger(issue.issue) && issue.issue > 0 &&
    typeof issue.provider === "string" &&
    typeof issue.project === "string" &&
    Number.isSafeInteger(issue.external_id) && issue.external_id >= 0
  );
}

function validDirectory(directory) {
  return (
    isObject(directory) &&
    nonEmptyString(directory.directory_id) &&
    nonEmptyString(directory.repo_identity) &&
    nonEmptyString(directory.checkout_path) &&
    nonEmptyString(directory.worktree_path) &&
    typeof directory.branch === "string" &&
    Number.isSafeInteger(directory.created_at) &&
    Array.isArray(directory.tasks) && directory.tasks.every(validTask) &&
    Array.isArray(directory.legacy_issues) && directory.legacy_issues.every(validLegacyIssue) &&
    Array.isArray(directory.sessions) && directory.sessions.every(nonEmptyString)
  );
}

function validUniqueDirectories(directories) {
  if (!Array.isArray(directories)) return false;
  const ids = new Set();
  for (const directory of directories) {
    if (!validDirectory(directory) || ids.has(directory.directory_id)) return false;
    ids.add(directory.directory_id);
  }
  return true;
}

async function readCompleteDirectoryCollection(
  argsForPage,
  cwd,
  contextField,
  expectedIssue,
  executePage,
) {
  const runPage = executePage || ((args) => safeText(phasegentCommand(args, cwd, EXECUTOR_ENV)));
  const directories = [];
  const directoryIds = new Set();
  let expectedContext;
  let totalCount;
  let pageCount;

  for (let page = 1; page <= MAX_DIRECTORY_PAGES; page += 1) {
    const args = [
      ...argsForPage(page),
      "--page", String(page),
      "--limit", String(DIRECTORY_PAGE_LIMIT),
    ];
    const result = await runPage(args);
    if (!result.ok || !result.value) return null;

    let parsed;
    try {
      parsed = JSON.parse(result.value);
    } catch (_) {
      return null;
    }
    if (!isObject(parsed) || !PAGING_FIELDS.some((field) => Object.hasOwn(parsed, field))) {
      return null;
    }

    if (
      parsed.page !== page ||
      parsed.limit !== DIRECTORY_PAGE_LIMIT ||
      !Array.isArray(parsed.items) ||
      parsed.items.length > DIRECTORY_PAGE_LIMIT ||
      typeof parsed.has_more !== "boolean" ||
      !Number.isSafeInteger(parsed.total_count) ||
      parsed.total_count < 0 ||
      parsed.total_count > DIRECTORY_PAGE_LIMIT * MAX_DIRECTORY_PAGES
    ) {
      return null;
    }

    const context = parsed[contextField];
    if (contextField === "issue") {
      if (!Number.isSafeInteger(context) || context !== expectedIssue) return null;
    } else if (!nonEmptyString(context)) {
      return null;
    }
    if (page === 1) {
      expectedContext = context;
      totalCount = parsed.total_count;
      pageCount = Math.max(1, Math.ceil(totalCount / DIRECTORY_PAGE_LIMIT));
      if (pageCount > MAX_DIRECTORY_PAGES) return null;
    } else if (context !== expectedContext || parsed.total_count !== totalCount) {
      return null;
    }

    if (page > pageCount) return null;
    const expectedItems = Math.min(DIRECTORY_PAGE_LIMIT, totalCount - directories.length);
    if (parsed.items.length !== expectedItems) return null;
    const expectedMore = directories.length + parsed.items.length < totalCount;
    if (parsed.has_more !== expectedMore) return null;

    for (const directory of parsed.items) {
      if (!validDirectory(directory) || directoryIds.has(directory.directory_id)) return null;
      directoryIds.add(directory.directory_id);
      directories.push(directory);
    }

    if (!parsed.has_more) {
      return directories.length === totalCount && page === pageCount ? directories : null;
    }
  }

  return null;
}

export async function readIssueDirectories(issueId, cwd, executePage) {
  const issue = Number(issueId);
  if (!Number.isSafeInteger(issue) || issue <= 0) return null;
  return await readCompleteDirectoryCollection(
    () => ["worktree", "status", "--issue", String(issue)],
    cwd,
    "issue",
    issue,
    executePage,
  );
}

export async function readRepositoryDirectories(cwd, executePage) {
  return await readCompleteDirectoryCollection(
    () => ["worktree", "list", "--no-sync"],
    cwd,
    "repo_identity",
    undefined,
    executePage,
  );
}

export function directoryAssociatesIssue(directory, issueId) {
  if (!isObject(directory) || !Number.isSafeInteger(issueId) || issueId <= 0) return false;
  return (
    (Array.isArray(directory.tasks) && directory.tasks.some((task) => task && task.issue === issueId)) ||
    (Array.isArray(directory.legacy_issues) &&
      directory.legacy_issues.some((issue) => issue && issue.issue === issueId))
  );
}

export function validBinding(binding) {
  return (
    isObject(binding) &&
    Number.isSafeInteger(binding.issueId) && binding.issueId > 0 &&
    nonEmptyString(binding.provider) &&
    nonEmptyString(binding.project) &&
    nonEmptyString(binding.externalId) &&
    Number.isSafeInteger(binding.taskId) && binding.taskId > 0 &&
    nonEmptyString(binding.instance)
  );
}

// A directory belongs to this checkout's bound issue only when one durable
// task association matches the immutable task id, instance, number, provider,
// project and external id exactly. Unknown legacy evidence is never treated as
// equivalent to a durable task.
export function directoryMatchesBinding(directory, binding) {
  if (!isObject(directory) || !validBinding(binding)) return false;
  const external = String(binding.externalId);
  return (
    Array.isArray(directory.tasks) &&
    directory.tasks.some(
      (task) =>
        task &&
        task.task_id === binding.taskId &&
        task.instance === binding.instance &&
        task.issue === binding.issueId &&
        task.provider === binding.provider &&
        task.project === binding.project &&
        task.external_id === external,
    )
  );
}

export function verifiedBindingDirectory(directories, binding, sessionId) {
  if (!Array.isArray(directories) || !validBinding(binding)) return null;
  const matches = directories.filter((directory) => directoryMatchesBinding(directory, binding));
  if (matches.length !== 1) return null;
  const directory = matches[0];
  const sessions = directory.sessions || [];
  if (sessions.length > 0 && !sessions.includes(sessionId)) return null;
  return directory.worktree_path;
}

// Number-only compatibility helper for callers that already proved the
// repository. Placement itself uses `verifiedBindingDirectory`.
export function verifiedIssueDirectory(directories, issueId, sessionId) {
  if (!Array.isArray(directories)) return null;
  const matches = directories.filter((directory) => directoryAssociatesIssue(directory, issueId));
  if (matches.length !== 1) return null;
  const directory = matches[0];
  const sessions = directory.sessions || [];
  if (sessions.length > 0 && !sessions.includes(sessionId)) return null;
  return directory.worktree_path;
}