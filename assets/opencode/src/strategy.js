// v2 worktree strategy. `editor.add` selects the strategy as the default, and
// the v2 editor has no way to wrap the host git strategy, so the strategy is
// only claimed when the checkout is already phasegent-bound; otherwise the host
// keeps its own git implementation.

import { acquireWorktree, readBranchBinding, readIssueLeases } from "./binding.js";
import { locationDirectory, phasegentCallsDisabled, safeText, warn } from "./runtime.js";

// Mirrors the host git strategy's command (packages/core/src/git.ts:657) so a
// failed lease never breaks worktree creation in a phasegent checkout.
export async function gitWorktreeAdd(input) {
  const directory = input && input.directory;
  const sourceDirectory = input && input.sourceDirectory;
  if (typeof directory !== "string" || directory.length === 0) {
    throw new Error("phasegent: worktree create requires a destination directory");
  }
  const ref = input && typeof input.branch === "string" && input.branch.length > 0
    ? input.branch
    : "HEAD";
  const command = Bun.$`git worktree add --detach -- ${directory} ${ref}`;
  const result = await safeText((sourceDirectory ? command.cwd(sourceDirectory) : command).quiet());
  if (!result.ok) {
    throw new Error(`phasegent: git worktree add failed: ${result.error}`);
  }
  return { directory };
}

export function worktreeStrategyDefinition(options) {
  const issueId = options.issueId;
  const fallbackDirectory = options.directory;
  return {
    id: "phasegent",
    async create(input) {
      const sourceDirectory = input && typeof input.sourceDirectory === "string"
        ? input.sourceDirectory
        : fallbackDirectory;
      const acquired = await options.acquire(issueId, null, sourceDirectory);
      if (acquired && typeof acquired.path === "string" && acquired.path.length > 0) {
        return { directory: acquired.path };
      }
      warn("phasegent: worktree acquire failed; falling back to a plain git worktree");
      return await options.gitAdd(input);
    },
    async remove() {
      // Retain the worktree and its branch; release/removal stays with
      // `phasegent worktree prune` so no lease is orphaned by a host action.
      return;
    },
    async list(sourceDirectory) {
      const root = typeof sourceDirectory === "string" && sourceDirectory.length > 0
        ? sourceDirectory
        : fallbackDirectory;
      const entries = [];
      const seen = new Set();
      const push = (directory, type) => {
        if (typeof directory !== "string" || directory.length === 0) return;
        if (seen.has(directory)) return;
        seen.add(directory);
        entries.push({ directory, type });
      };
      push(root, "root");
      const leases = await options.readLeases(issueId, root);
      for (const lease of leases || []) {
        if (!lease || typeof lease !== "object") continue;
        push(lease.worktree_path, "worktree");
      }
      return entries;
    },
  };
}

// `deps` is an internal seam so tests can exercise the strategy without the
// phasegent CLI; production callers pass nothing. `PHASEGENT_WORKTREE_NO_DISCOVER=1`
// keeps the adapter from running the CLI at all, and then the host keeps its own
// git strategy. The binding is probed once per plugin instance: a checkout that
// gains its binding later keeps the host git strategy until the plugin reloads.
export async function registerWorktreeStrategy(context, deps) {
  if (phasegentCallsDisabled()) return null;
  const worktree = context && context.worktree;
  const transform = worktree && worktree.transform;
  if (typeof transform !== "function") {
    warn("phasegent: host exposes no worktree.transform; the git strategy stays in place");
    return null;
  }
  const directory = locationDirectory(context);
  const readBinding = (deps && deps.readBinding) || readBranchBinding;
  const issueId = await readBinding(directory);
  if (!issueId) return null;
  const definition = worktreeStrategyDefinition({
    issueId,
    directory,
    acquire: (deps && deps.acquire) || acquireWorktree,
    gitAdd: (deps && deps.gitAdd) || gitWorktreeAdd,
    readLeases: (deps && deps.readLeases) || readIssueLeases,
  });
  return await transform((editor) => {
    editor.add(definition);
  });
}
