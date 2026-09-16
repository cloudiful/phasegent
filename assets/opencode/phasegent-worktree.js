// phasegent:managed
// Installed by `phasegent plugin install [--global|--project]`; safe to reinstall or remove.
// Auto-acquires a per-(repo, issue, session) worktree when an OpenCode session starts.
// Once a session lands on that worktree, `tool.execute.before` redirects relative path
// arguments and a bare/relative bash `workdir` into it, so Task-spawned sub-agents cannot
// silently drift back to the original checkout. Absolute paths pass through untouched: an
// explicit escape, and the external_directory permission check that guards it, are never
// rewritten. All worktree calls stay local: no network, no credentials, no .env copies.
// Branches and directories are never deleted by this adapter; removal is the explicit
// `phasegent worktree prune` CLI job. V1 API only (experimental_workspace /
// tool.execute.before); issue #440 builds on Phase 3 of issue #239.

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

// ---------------------------------------------------------------------------
// Acquired-worktree registry (issue #440).
//
// `target` resolves the worktree for a workspace, but there is no per-session
// workspace identity: Task-spawned sub-agents run with a different sessionID and
// never call `target` themselves. Keep the per-session mapping when the runtime
// reports one and fall back to the most recently acquired worktree so those
// sub-agents are redirected too. An empty registry means "no worktree", and the
// hook then leaves every tool argument untouched.
// ---------------------------------------------------------------------------

const sessionWorktrees = new Map();
let activeWorktree = null;

function rememberWorktree(sessionId, directory) {
  if (typeof directory !== "string" || directory.length === 0) return;
  activeWorktree = directory;
  if (sessionId !== undefined && sessionId !== null) {
    sessionWorktrees.set(String(sessionId), directory);
  }
}

function worktreeForSession(sessionId) {
  if (sessionId !== undefined && sessionId !== null) {
    const known = sessionWorktrees.get(String(sessionId));
    if (known) return known;
  }
  return activeWorktree;
}

function resetWorktrees() {
  sessionWorktrees.clear();
  activeWorktree = null;
}

// ---------------------------------------------------------------------------
// Pure redirect helpers (issue #440).
//
// Only relative values are rewritten; everything absolute (POSIX, Windows drive
// or UNC) is returned verbatim so an explicit escape is never silently
// retargeted and the external_directory check still sees the path the model
// asked for.
// ---------------------------------------------------------------------------

function isAbsolutePath(value) {
  if (typeof value !== "string" || value.length === 0) return false;
  if (value.startsWith("/")) return true;
  if (/^[A-Za-z]:[\\/]/.test(value)) return true;
  return value.startsWith("\\\\");
}

function redirectPathValue(workdir, value) {
  if (typeof workdir !== "string" || workdir.length === 0) return value;
  if (typeof value !== "string" || value.length === 0) return value;
  if (isAbsolutePath(value)) return value;
  return `${workdir.replace(/[\\/]+$/, "")}/${value.replace(/^[\\/]+/, "")}`;
}

// Tool arguments that carry a filesystem path. Tools without an entry are
// never rewritten.
const PATH_ARG_KEYS = {
  read: ["filePath"],
  write: ["filePath"],
  edit: ["filePath"],
  glob: ["path"],
  grep: ["path"],
};

function redirectArgs(tool, workdir, args) {
  if (!args || typeof args !== "object") return args;
  // No acquired worktree: pass the call through byte-for-byte.
  if (typeof workdir !== "string" || workdir.length === 0) return args;
  const redirected = { ...args };
  const keys = PATH_ARG_KEYS[tool];
  if (keys) {
    for (const key of keys) {
      if (typeof redirected[key] === "string") {
        redirected[key] = redirectPathValue(workdir, redirected[key]);
      }
    }
  }
  if (tool === "bash") {
    const current = redirected.workdir;
    if (typeof current === "string" && current.length > 0) {
      // A relative workdir resolves against the worktree; an absolute one stays.
      redirected.workdir = redirectPathValue(workdir, current);
    } else {
      // Bare bash: the shell would otherwise default to the stale session cwd.
      redirected.workdir = workdir;
    }
  }
  return redirected;
}

function createRedirectHook() {
  return {
    "tool.execute.before": async (input, output) => {
      const workdir = worktreeForSession(input && input.sessionID);
      if (!workdir || !output || !output.args) return;
      const redirected = redirectArgs(input.tool, workdir, output.args);
      if (redirected === output.args) return;
      for (const key of Object.keys(redirected)) output.args[key] = redirected[key];
    },
  };
}

// ---------------------------------------------------------------------------
// Workspace adapter (unchanged shape from Phase 3 / issue #239).
// ---------------------------------------------------------------------------

async function registerWorkspace(workspace) {
  const api =
    workspace && typeof workspace.register === "function"
      ? workspace
      : typeof experimental_workspace !== "undefined" &&
          experimental_workspace &&
          typeof experimental_workspace.register === "function"
        ? experimental_workspace
        : null;
  if (!api) return false;
  await api.register("phasegent", {
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
        rememberWorktree(sessionID, workdir);
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

// V1 plugin entry point. The registered adapter resolves the worktree; the
// returned hook redirects later tool calls into it.
export const PhasegentWorktreePlugin = async ({ experimental_workspace } = {}) => {
  try {
    await registerWorkspace(experimental_workspace);
  } catch (_) {
    // registration is best-effort; a failure must not disable the redirect hook
  }
  return createRedirectHook();
};

// Helpers are attached to the exported plugin rather than exported as their own
// bindings: the legacy loader treats every module export as a plugin factory, and
// these would then be invoked with a PluginInput.
PhasegentWorktreePlugin.redirect = Object.freeze({
  isAbsolutePath,
  redirectPathValue,
  redirectArgs,
  rememberWorktree,
  worktreeForSession,
  resetWorktrees,
  createRedirectHook,
});

export default PhasegentWorktreePlugin;

// Compatibility with loaders that expose `experimental_workspace` as a module
// global instead of passing it to the plugin factory. A no-op on the V1 API
// targeted here, where the variable is undefined.
registerWorkspace().catch(() => {
  // nothing to do when the workspace API is absent
});
