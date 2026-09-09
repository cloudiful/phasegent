// phasegent:managed
// Installed by `phasegent plugin install [--global|--project]`; safe to reinstall or remove.
// Auto-acquires a per-(repo, issue, session) worktree when an OpenCode session starts.
// All worktree calls stay local: no network, no credentials, no .env copies.
// Branches and directories are never deleted by this adapter; removal is the
// explicit `phasegent worktree prune` CLI job. Phase 3 of issue #239.

async function safeText(command) {
  try {
    const text = (await command.text()).trim();
    return { ok: true, value: text };
  } catch (error) {
    return { ok: false, error: String(error && error.message ? error.message : error) };
  }
}

async function readBranchBinding() {
  const result = await safeText(Bun.$`phasegent issue status`.quiet());
  if (!result.ok || !result.value) return null;
  try {
    const parsed = JSON.parse(result.value);
    if (parsed && typeof parsed.issue_id === "number" && parsed.issue_id > 0) {
      return parsed.issue_id;
    }
  } catch (_) {
    // not JSON or partial output; treat as no binding
  }
  return null;
}

async function acquireWorktree(issueId, sessionId) {
  const args = [
    "--role", "orchestrator",
    "worktree", "acquire",
    "--issue", String(issueId),
    "--format", "json",
  ];
  if (sessionId) args.push("--session", String(sessionId));
  const result = await safeText(Bun.$`phasegent ${args}`.quiet());
  if (!result.ok || !result.value) return null;
  try {
    const parsed = JSON.parse(result.value);
    if (parsed && typeof parsed.path === "string" && parsed.path.length > 0) {
      return parsed;
    }
  } catch (_) {
    // ignore
  }
  return null;
}

async function registerWorkspace() {
  if (
    typeof experimental_workspace === "undefined" ||
    !experimental_workspace ||
    typeof experimental_workspace.register !== "function"
  ) {
    return false;
  }
  await experimental_workspace.register("phasegent", {
    name: "phasegent",
    description:
      "Acquire a per-(repo, issue, session) worktree lease via `phasegent worktree acquire`; " +
      "falls back to the original directory when no issue binding is present or acquire fails.",
    async configure({ directory }) {
      return { directory };
    },
    async create({ directory }) {
      // worktree add already creates the directory; this is a best-effort
      // mkdir -p so a session bootstrap that lands here before the lease
      // table resolves does not race the filesystem.
      try {
        await Bun.$`mkdir -p ${directory}`.quiet();
      } catch (_) {
        // best-effort: ignore failures so a non-writable parent never blocks the session
      }
      return { directory };
    },
    async remove(_arg) {
      // Phase 3 contract: do NOT delete the directory or branch.
      // Retain the worktree for explicit `phasegent worktree prune` via CLI.
      return;
    },
    async target({ directory, sessionID }) {
      let workdir = directory;
      let warning = null;
      try {
        const gitCheck = await safeText(
          Bun.$`git rev-parse --git-common-dir`.quiet(),
        );
        if (!gitCheck.ok || !gitCheck.value) {
          warning = "phasegent: not a git checkout; reusing original directory";
          const out = { type: "local", directory: workdir };
          if (warning) out.warning = warning;
          return out;
        }
        const issueId = await readBranchBinding();
        if (!issueId) {
          warning =
            "phasegent: no branch issue binding; reusing original directory";
          const out = { type: "local", directory: workdir };
          if (warning) out.warning = warning;
          return out;
        }
        const acquired = await acquireWorktree(issueId, sessionID);
        if (!acquired || !acquired.path) {
          warning =
            "phasegent: worktree acquire failed; reusing original directory";
          const out = { type: "local", directory: workdir };
          if (warning) out.warning = warning;
          return out;
        }
        workdir = acquired.path;
      } catch (_) {
        warning =
          "phasegent: unexpected adapter failure; reusing original directory";
      }
      const out = { type: "local", directory: workdir };
      if (warning) out.warning = warning;
      return out;
    },
  });
  return true;
}

registerWorkspace().catch(() => {
  // experimental_workspace may be absent in this OpenCode build; nothing to do.
});