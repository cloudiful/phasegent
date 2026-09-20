// phasegent:managed
// Installed by `phasegent plugin install [--global|--project]`; safe to reinstall or remove.
//
// OpenCode v2 worktree adapter. OpenCode >= 2.0 is required: the v1 plugin shape is
// rejected by the v2 module loader (`PluginModule.LoadError: Plugin must export a
// default definition with an id and an effect or setup function.`,
// packages/core/src/plugin/module.ts:60-73, :111). The v2 contract is
// `export default { id, setup }`; `setup(context)` registers hooks imperatively and
// returns a cleanup (packages/plugin/src/promise/plugin.ts:56-61).
//
// v2 registrations replace the v1 workspace adapter:
//   * `context.tool.hook("execute.before", event)` — one mutable event
//     `{ tool, sessionID, agent, messageID, id, input }`; core continues with the
//     returned `event.input` (packages/core/src/tool.ts:103-111, :271-280), so
//     relative path arguments and a bare/relative shell `workdir` are rewritten in
//     place.
//   * `context.worktree.transform(editor => editor.add({ id, create, remove, list }))`
//     (packages/plugin/src/promise/worktree.ts:5-22). It has no v1 `target`
//     callback, so the acquired worktree becomes the session directory through
//     `context.session.move` (packages/core/src/session/move.ts:41-49). The strategy
//     is only claimed when the checkout already carries a phasegent issue binding,
//     so a non-phasegent project keeps the host git strategy.
//
// Session identity is `event.sessionID`. Degradation is deliberate: no binding or a
// failed acquire keeps the original directory and warns on the console (v2 has no
// structured warning channel). Absolute paths pass through untouched, so an explicit
// escape and the external_directory check that guards it are never rewritten. All
// worktree calls stay local: no network, no credentials, no .env copies. Branches
// and directories are never deleted here; removal is `phasegent worktree prune`.

function errorText(error) {
  return String(error && error.message ? error.message : error);
}

// v2 has no structured warning field on the plugin context, so degradation is
// reported on the host console. Logging never throws into a tool call.
function warn(message) {
  try {
    console.warn(message);
  } catch (_) {
    // ignore: a broken console must not break path redirection
  }
}

async function safeText(command) {
  try {
    const text = (await command.text()).trim();
    return { ok: true, value: text };
  } catch (error) {
    return { ok: false, error: errorText(error) };
  }
}

function phasegentCallsDisabled() {
  try {
    return Boolean(process && process.env && process.env.PHASEGENT_WORKTREE_NO_DISCOVER === "1");
  } catch (_) {
    return false;
  }
}

// `args` is spread into separate argv entries by Bun's shell interpolation.
function phasegentCommand(args, cwd) {
  const command = Bun.$`phasegent ${args}`;
  return (cwd ? command.cwd(cwd) : command).quiet();
}

function locationDirectory(context) {
  const location = context && context.location;
  return location && typeof location.directory === "string" ? location.directory : null;
}

async function readBranchBinding(cwd) {
  const result = await safeText(phasegentCommand(["issue", "status"], cwd));
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

async function acquireWorktree(issueId, sessionId, cwd) {
  const args = [
    "--role", "orchestrator",
    "worktree", "acquire",
    "--issue", String(issueId),
    "--format", "json",
  ];
  if (sessionId) args.push("--session", String(sessionId));
  const result = await safeText(phasegentCommand(args, cwd));
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

async function readIssueLeases(issueId, cwd) {
  const args = ["--role", "executor", "worktree", "status", "--issue", String(issueId)];
  const result = await safeText(phasegentCommand(args, cwd));
  if (!result.ok || !result.value) return null;
  try {
    const parsed = JSON.parse(result.value);
    return parsed && Array.isArray(parsed.leases) ? parsed.leases : null;
  } catch (_) {
    return null;
  }
}

// ---------------------------------------------------------------------------
// Session injection + lazy mid-session discovery (issue #18, Task 2).
//
// `issue create`/`issue bind` auto-acquire a worktree on conflict when they
// carry `--session`; the plugin owns the session id (`event.sessionID`) so the
// model never mints one by hand. `injectSessionIntoPhasegentCommand` is pure and
// idempotent; `discoverWorktreeForSession` is best-effort and every failure falls
// through silently so the hook degrades to passthrough.
// ---------------------------------------------------------------------------

function injectSessionIntoPhasegentCommand(command, sessionId) {
  if (typeof command !== "string" || !sessionId) return command;
  if (!/phasegent\b.*\bissue\s+(create|bind)\b/.test(command)) return command;
  if (/(^|\s)--session(\s|=)/.test(command)) return command;
  return `${command} --session ${sessionId}`;
}

function pickActiveWorktreePath(leases) {
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

async function discoverWorktreeForSession(sessionId, cwd) {
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

// ---------------------------------------------------------------------------
// Acquired-worktree registry (issue #440).
//
// `ctx.session.move` relocates a session, but there is no per-session worktree
// identity on the v2 worktree domain: Task-spawned sub-agents run with a
// different sessionID and never resolve one themselves. Keep the per-session
// mapping when the runtime reports one and fall back to the most recently
// acquired worktree so those sub-agents are redirected too. An empty registry
// means "no worktree", and the hook then leaves every tool argument untouched.
// ---------------------------------------------------------------------------

const sessionWorktrees = new Map();
let activeWorktree = null;
const movedSessions = new Set();

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
  movedSessions.clear();
  activeWorktree = null;
}

// `context.session.move` hands an active runner the placement at its next step
// boundary (packages/core/src/session/move.ts:114-155). It is attempted once per
// session: repeated calls would enqueue repeated inbox items.
async function moveSessionToWorktree(context, sessionId, directory) {
  if (!context) return;
  if (typeof directory !== "string" || directory.length === 0) return;
  if (sessionId === undefined || sessionId === null) return;
  const key = String(sessionId);
  if (movedSessions.has(key)) return;
  movedSessions.add(key);
  const current = locationDirectory(context);
  if (current === directory) return;
  const move = context && context.session && context.session.move;
  if (typeof move !== "function") {
    warn("phasegent: host exposes no session.move; redirecting tool arguments only");
    return;
  }
  try {
    await move({ sessionID: sessionId, directory });
  } catch (error) {
    warn(
      `phasegent: session move to ${directory} failed; redirecting tool arguments instead (${errorText(error)})`,
    );
  }
}

// Resolve (or acquire) the worktree for a session. A missing binding silently
// keeps the original directory; a failed acquire warns and does the same.
async function ensureSessionWorktree(context, sessionId) {
  if (!sessionId) return null;
  const known = worktreeForSession(sessionId);
  if (known) {
    await moveSessionToWorktree(context, sessionId, known);
    return known;
  }
  if (phasegentCallsDisabled()) return null;
  const cwd = locationDirectory(context);
  try {
    const discovered = await discoverWorktreeForSession(sessionId, cwd);
    if (discovered) {
      await moveSessionToWorktree(context, sessionId, discovered);
      return discovered;
    }
    const issueId = await readBranchBinding(cwd);
    if (!issueId) return null;
    const acquired = await acquireWorktree(issueId, sessionId, cwd);
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

// ---------------------------------------------------------------------------
// Pure redirect helpers (issue #440, v2 argument names).
//
// Only relative values are rewritten; everything absolute (POSIX, Windows drive
// or UNC) is returned verbatim so an explicit escape is never silently
// retargeted and the external_directory check still sees the path the model
// asked for. v2 renamed the file tools' `filePath` to `path` and the shell tool
// from `bash` to `shell` (packages/core/src/tool/plugin/{read,write,edit}.ts,
// tool/shell.ts:22).
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
// never rewritten. `bash` is kept as a shell alias for older tool registrations.
const PATH_ARG_KEYS = {
  read: ["path"],
  write: ["path"],
  edit: ["path"],
  glob: ["path"],
  grep: ["path"],
};

const SEARCH_TOOLS = ["glob", "grep"];
const SHELL_TOOLS = ["shell", "bash"];

function redirectArgs(tool, workdir, args, sessionId) {
  if (!args || typeof args !== "object") return args;
  // No acquired worktree: pass the call through byte-for-byte.
  if (typeof workdir !== "string" || workdir.length === 0) return args;
  const redirected = { ...args };
  const keys = PATH_ARG_KEYS[tool];
  if (SEARCH_TOOLS.includes(tool)) {
    const current = redirected.path;
    if (typeof current !== "string" || current.length === 0) {
      redirected.path = workdir;
    } else {
      redirected.path = redirectPathValue(workdir, current);
    }
  } else if (keys) {
    for (const key of keys) {
      if (typeof redirected[key] === "string") {
        redirected[key] = redirectPathValue(workdir, redirected[key]);
      }
    }
  }
  if (SHELL_TOOLS.includes(tool)) {
    const current = redirected.workdir;
    if (typeof current === "string" && current.length > 0) {
      // A relative workdir resolves against the worktree; an absolute one stays.
      redirected.workdir = redirectPathValue(workdir, current);
    } else {
      // Bare shell: the shell would otherwise default to the stale session cwd.
      redirected.workdir = workdir;
    }
    if (
      typeof redirected.command === "string" &&
      sessionId !== undefined &&
      sessionId !== null &&
      String(sessionId).length > 0
    ) {
      redirected.command = injectSessionIntoPhasegentCommand(
        redirected.command,
        sessionId,
      );
    }
  }
  return redirected;
}

// ---------------------------------------------------------------------------
// v2 worktree strategy. `editor.add` selects the strategy as the default, and
// the v2 editor has no way to wrap the host git strategy, so the strategy is
// only claimed when the checkout is already phasegent-bound; otherwise the host
// keeps its own git implementation.
// ---------------------------------------------------------------------------

// Mirrors the host git strategy's command (packages/core/src/git.ts:657) so a
// failed lease never breaks worktree creation in a phasegent checkout.
async function gitWorktreeAdd(input) {
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

function worktreeStrategyDefinition(options) {
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
async function registerWorktreeStrategy(context, deps) {
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

// ---------------------------------------------------------------------------
// v2 tool hook: one mutable event per call.
// ---------------------------------------------------------------------------

function createRedirectHook(context) {
  return async function executeBefore(event) {
    const sessionId = event ? event.sessionID : undefined;
    const input = event ? event.input : undefined;
    // Lazy mid-session discovery: the registry may be empty when the session
    // was created before the first tool call or when a Task-spawned sub-agent
    // arrives with a fresh session id. Best-effort only.
    let workdir = null;
    try {
      workdir = await ensureSessionWorktree(context, sessionId);
    } catch (_) {
      workdir = null; // silent passthrough: a failed lookup must not block the call
    }
    if (!input || typeof input !== "object") return;
    // Session injection for `issue create|bind` runs even without a worktree so
    // the CLI can auto-acquire on conflict (issue #18 Task 2 helper).
    if (SHELL_TOOLS.includes(event.tool) && typeof input.command === "string" && sessionId) {
      try {
        const injected = injectSessionIntoPhasegentCommand(input.command, sessionId);
        if (injected !== input.command) input.command = injected;
      } catch (_) {
        // silent passthrough
      }
    }
    if (typeof workdir !== "string" || workdir.length === 0) return;
    const redirected = redirectArgs(event.tool, workdir, input, sessionId);
    if (redirected === input) return;
    // Mutate in place: core keeps using `event.input`, and in-place writes keep
    // the object identity the caller already holds.
    for (const key of Object.keys(redirected)) input[key] = redirected[key];
  };
}

// ---------------------------------------------------------------------------
// v2 plugin entry point (`export default { id, setup }`). Registrations are
// disposed by the cleanup `setup` returns.
// ---------------------------------------------------------------------------

const PhasegentWorktreePlugin = {
  id: "phasegent-worktree",
  async setup(context) {
    const registrations = [];
    try {
      const strategy = await registerWorktreeStrategy(context);
      if (strategy) registrations.push(strategy);
    } catch (error) {
      warn(`phasegent: worktree strategy registration failed (${errorText(error)})`);
    }
    try {
      const hook = context && context.tool && context.tool.hook;
      if (typeof hook === "function") {
        registrations.push(await hook("execute.before", createRedirectHook(context)));
      } else {
        warn("phasegent: host exposes no tool hook; path redirection is disabled");
      }
    } catch (error) {
      warn(`phasegent: tool.execute.before registration failed (${errorText(error)})`);
    }
    return async () => {
      for (const registration of registrations) {
        try {
          if (registration && typeof registration.dispose === "function") {
            await registration.dispose();
          }
        } catch (_) {
          // disposal is best-effort
        }
      }
    };
  },
};

// Helpers are attached to the plugin object rather than exported as their own
// bindings so the loader only ever sees one `default` export (extra keys are
// ignored by the module schema).
PhasegentWorktreePlugin.redirect = Object.freeze({
  isAbsolutePath,
  redirectPathValue,
  redirectArgs,
  rememberWorktree,
  worktreeForSession,
  resetWorktrees,
  createRedirectHook,
  injectSessionIntoPhasegentCommand,
  pickActiveWorktreePath,
  discoverWorktreeForSession,
  ensureSessionWorktree,
  moveSessionToWorktree,
  readBranchBinding,
  acquireWorktree,
  readIssueLeases,
  registerWorktreeStrategy,
  worktreeStrategyDefinition,
  gitWorktreeAdd,
});

export default PhasegentWorktreePlugin;
