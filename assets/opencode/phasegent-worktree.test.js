// Focused tests for the phasegent worktree plugin (issue #532, v2 contract).
//
// The helpers are pure or dependency-injected so they run under `bun test`
// without OpenCode and without the phasegent CLI: the source entry is imported
// directly and only its default export plus the attached `redirect` helpers are
// touched. The generated `phasegent-worktree.js` dist is covered by the
// freshness assertions at the bottom of this file; the install/status/uninstall
// marker behaviour is covered by the Rust asset assertions in
// `src/plugin_tests.rs`.

import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import { spawnSync } from "node:child_process";
import { chmod, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { buildPlugin } from "./src/build.js";
import PhasegentWorktreePlugin from "./src/index.js";

const {
  isAbsolutePath,
  redirectPathValue,
  redirectPaths,
  agentRole,
  isSubagentSession,
  sessionPlaced,
  rewritePhasegentCommand,
  scanHeredocRanges,
  maskQuoted,
  shellSegments,
  transformCodeOnly,
  phasegentInvocation,
  rememberWorktree,
  worktreeForSession,
  resetWorktrees,
  createRedirectHook,
  createPromptHook,
  createManagedShellBridge,
  createShellEnvHook,
  createToolAfterHook,
  isShellHookSupported,
  pickActiveWorktreePath,
  issueClosedLocally,
  ensureSessionWorktree,
  readSessionInfo,
  readAuthoritativeDirectory,
  reconcileSessionLocation,
  inheritedWorktree,
  acquireWorktree,
  registerWorktreeStrategy,
  worktreeStrategyDefinition,
  registerSkill,
  skillDefinition,
  roleSkillDefinitions,
  skillDefinitions,
  registerAgentSkills,
  roleSkillId,
  roleSkillContent,
  withSkillPrefix,
} = PhasegentWorktreePlugin.redirect;

const WORKTREE = "/repo/.worktrees/issue-532";

// The generated Windows role scope (issue #588 P2): a `$( … )` subexpression
// that scopes `PHASEGENT_ROLE` to the CLI child, restores the previous value
// from `finally`, and re-asserts a failing CLI status after the restore.
const WINDOWS_SCOPE_HEAD =
  "$( $__phasegent_role=$env:PHASEGENT_ROLE; try { $env:PHASEGENT_ROLE='executor'; ";
const WINDOWS_SCOPE_TAIL =
  " } finally { $__phasegent_status=$LASTEXITCODE; $env:PHASEGENT_ROLE=$__phasegent_role; " +
  "if ($__phasegent_status -ne 0) { Write-Error -Message 'phasegent failed' -ErrorAction SilentlyContinue } } )";
const windowsScope = (command) => `${WINDOWS_SCOPE_HEAD}${command}${WINDOWS_SCOPE_TAIL}`;

// The PowerShell runtime regressions need a PowerShell 7 host; they are skipped
// where none is installed (CI on Linux) and run locally via `PHASEGENT_PWSH`.
const PWSH_PATH =
  process.env.PHASEGENT_PWSH ||
  (typeof Bun !== "undefined" && typeof Bun.which === "function" ? Bun.which("pwsh") : null);

function restoreNoDiscover(saved) {
  if (saved === undefined) {
    delete process.env.PHASEGENT_WORKTREE_NO_DISCOVER;
  } else {
    process.env.PHASEGENT_WORKTREE_NO_DISCOVER = saved;
  }
}

// `discover`/`readBinding` are stubbed so a missed placement fails loudly
// instead of spawning the phasegent CLI; `readSessionInfo` and the
// authoritative read stay real and read the fake host. The lazy path never
// creates a worktree on its own (issue 616), so there is no acquire seam to
// stub.
const noCliDeps = {
  discover: async () => null,
  readBinding: async () => 567,
  readLeaseHistory: async () => [],
};

function hostContext(sessions, moves) {
  return {
    location: { directory: "/repo" },
    session: {
      move: async (input) => moves.push(input),
      get: async ({ sessionID }) => sessions[sessionID],
    },
  };
}

function promptEvent(sessionID) {
  return { sessionID, messageID: "msg-1", prompt: { text: "do the phase" }, delivery: "steer" };
}

// Capture the adapter's console warnings for the duration of one assertion.
async function withWarnings(run) {
  const warnings = [];
  const original = console.warn;
  console.warn = (message) => warnings.push(String(message));
  try {
    await run();
  } finally {
    console.warn = original;
  }
  return warnings;
}

beforeEach(() => {
  resetWorktrees();
});

describe("plugin module shape (v2)", () => {
  test("default export is a plugin definition with id and setup", () => {
    expect(PhasegentWorktreePlugin).toBeObject();
    expect(PhasegentWorktreePlugin.id).toBe("phasegent-worktree");
    expect(typeof PhasegentWorktreePlugin.setup).toBe("function");
    expect(PhasegentWorktreePlugin.redirect).toBeObject();
  });

  test("setup registers the tool hook, the prompt hook and skill, and returns a cleanup", async () => {
    const saved = process.env.PHASEGENT_WORKTREE_NO_DISCOVER;
    process.env.PHASEGENT_WORKTREE_NO_DISCOVER = "1";
    try {
      const registered = [];
      const context = {
        location: { directory: "/repo" },
        session: {
          move: async () => {},
          hook: async (name, callback) => {
            registered.push({ name, callback });
            return { dispose: async () => {} };
          },
        },
        worktree: {
          transform: async () => ({ dispose: async () => {} }),
        },
        tool: {
          hook: async (name, callback) => {
            registered.push({ name, callback });
            return { dispose: async () => {} };
          },
        },
        skill: {
          transform: async () => {
            registered.push({ name: "skill.transform" });
            return { dispose: async () => {} };
          },
        },
        agent: {
          transform: async (callback) => {
            registered.push({ name: "agent.transform" });
            await callback({
              list: () => [{ id: "executor", system: "" }],
              update: (id, mutate) => mutate({ id, system: "" }),
            });
            return { dispose: async () => {} };
          },
        },
      };
      const cleanup = await PhasegentWorktreePlugin.setup(context);
      expect(registered.map((item) => item.name)).toEqual([
        "prompt",
        "execute.before",
        "skill.transform",
        "agent.transform",
      ]);
      expect(typeof registered[0].callback).toBe("function");
      expect(typeof registered[1].callback).toBe("function");
      expect(typeof cleanup).toBe("function");
      await cleanup();
    } finally {
      restoreNoDiscover(saved);
    }
  });

  test("setup keeps the tool hook when the host exposes no session prompt hook", async () => {
    const saved = process.env.PHASEGENT_WORKTREE_NO_DISCOVER;
    process.env.PHASEGENT_WORKTREE_NO_DISCOVER = "1";
    const warnings = [];
    const original = console.warn;
    console.warn = (message) => warnings.push(String(message));
    try {
      const registered = [];
      const cleanup = await PhasegentWorktreePlugin.setup({
        location: { directory: "/repo" },
        session: { move: async () => {} },
        tool: {
          hook: async (name) => {
            registered.push(name);
            return { dispose: async () => {} };
          },
        },
      });
      expect(registered).toEqual(["execute.before"]);
      expect(warnings.join("\n")).toContain("no session prompt hook");
      expect(typeof cleanup).toBe("function");
      await cleanup();
    } finally {
      console.warn = original;
      restoreNoDiscover(saved);
    }
  });

  test("setup still returns a cleanup when every registration fails", async () => {
    const context = {
      worktree: {
        transform: async () => {
          throw new Error("transform unavailable");
        },
      },
      tool: {
        hook: async () => {
          throw new Error("hook unavailable");
        },
      },
      skill: {
        transform: async () => {
          throw new Error("skill transform unavailable");
        },
      },
      agent: {
        transform: async () => {
          throw new Error("agent transform unavailable");
        },
      },
    };
    const cleanup = await PhasegentWorktreePlugin.setup(context);
    expect(typeof cleanup).toBe("function");
    await cleanup();
  });

  test("setup subscribes to the event stream for question notifications and disposes it", async () => {
    const saved = process.env.PHASEGENT_WORKTREE_NO_DISCOVER;
    process.env.PHASEGENT_WORKTREE_NO_DISCOVER = "1";
    try {
      const stream = {
        [Symbol.asyncIterator]() {
          return {
            next() {
              return new Promise(() => {});
            },
            return() {
              return Promise.resolve({ value: undefined, done: true });
            },
          };
        },
      };
      let subscribeOptions = null;
      const context = {
        location: { directory: "/repo" },
        event: {
          subscribe: (options) => {
            subscribeOptions = options;
            return stream;
          },
        },
      };
      const cleanup = await PhasegentWorktreePlugin.setup(context);
      expect(typeof subscribeOptions).toBe("object");
      expect(subscribeOptions.signal).toBeDefined();
      let aborted = false;
      subscribeOptions.signal.addEventListener("abort", () => {
        aborted = true;
      });
      expect(typeof cleanup).toBe("function");
      await cleanup();
      expect(aborted).toBe(true);
    } finally {
      restoreNoDiscover(saved);
    }
  });
});

describe("isAbsolutePath", () => {
  test("detects POSIX, drive and UNC absolutes", () => {
    expect(isAbsolutePath("/etc/hosts")).toBe(true);
    expect(isAbsolutePath("/")).toBe(true);
    expect(isAbsolutePath("C:\\repo\\file.rs")).toBe(true);
    expect(isAbsolutePath("c:/repo/file.rs")).toBe(true);
    expect(isAbsolutePath("\\\\server\\share\\file")).toBe(true);
  });

  test("treats relative and empty values as relative", () => {
    expect(isAbsolutePath("src/a.rs")).toBe(false);
    expect(isAbsolutePath("./src/a.rs")).toBe(false);
    expect(isAbsolutePath("../a.rs")).toBe(false);
    expect(isAbsolutePath("")).toBe(false);
    expect(isAbsolutePath(undefined)).toBe(false);
    expect(isAbsolutePath(42)).toBe(false);
  });
});

describe("redirectPathValue", () => {
  test("joins relative values onto the worktree", () => {
    expect(redirectPathValue(WORKTREE, "src/a.rs")).toBe(`${WORKTREE}/src/a.rs`);
    expect(redirectPathValue(WORKTREE, "./src/a.rs")).toBe(`${WORKTREE}/./src/a.rs`);
  });

  test("leaves absolute values untouched", () => {
    expect(redirectPathValue(WORKTREE, "/etc/hosts")).toBe("/etc/hosts");
    expect(redirectPathValue(WORKTREE, "C:\\repo\\a.rs")).toBe("C:\\repo\\a.rs");
  });

  test("passes through when the worktree or value is empty", () => {
    expect(redirectPathValue(null, "src/a.rs")).toBe("src/a.rs");
    expect(redirectPathValue("", "src/a.rs")).toBe("src/a.rs");
    expect(redirectPathValue(WORKTREE, "")).toBe("");
    expect(redirectPathValue(WORKTREE, undefined)).toBeUndefined();
  });

  test("normalises a trailing worktree separator", () => {
    expect(redirectPathValue("/repo/wt/", "src/a.rs")).toBe("/repo/wt/src/a.rs");
  });
});

describe("redirectPaths file tools (v2 `path` argument)", () => {
  test("redirects a relative path for read/write/edit", () => {
    for (const tool of ["read", "write", "edit"]) {
      const args = { path: "src/a.rs" };
      expect(redirectPaths(tool, WORKTREE, args).path).toBe(`${WORKTREE}/src/a.rs`);
    }
  });

  test("redirects a relative path for glob/grep and leaves the pattern", () => {
    for (const tool of ["glob", "grep"]) {
      const args = { pattern: "*.rs", path: "src" };
      const out = redirectPaths(tool, WORKTREE, args);
      expect(out.path).toBe(`${WORKTREE}/src`);
      expect(out.pattern).toBe("*.rs");
    }
  });

  test("defaults a missing glob/grep path to the worktree", () => {
    for (const tool of ["glob", "grep"]) {
      expect(redirectPaths(tool, WORKTREE, {}).path).toBe(WORKTREE);
      expect(redirectPaths(tool, WORKTREE, { pattern: "*.rs" }).path).toBe(WORKTREE);
    }
  });

  test("defaults an empty or non-string glob/grep path to the worktree", () => {
    expect(redirectPaths("glob", WORKTREE, { path: "" }).path).toBe(WORKTREE);
    expect(redirectPaths("grep", WORKTREE, { path: "" }).path).toBe(WORKTREE);
    expect(redirectPaths("glob", WORKTREE, { path: 42 }).path).toBe(WORKTREE);
    expect(redirectPaths("grep", WORKTREE, { path: null }).path).toBe(WORKTREE);
  });

  test("joins relative glob/grep paths and passes absolute paths through", () => {
    for (const tool of ["glob", "grep"]) {
      expect(redirectPaths(tool, WORKTREE, { path: "src" }).path).toBe(`${WORKTREE}/src`);
      expect(
        redirectPaths(tool, WORKTREE, { path: "/home/dev/codes/tools/phasegent/src" }).path,
      ).toBe("/home/dev/codes/tools/phasegent/src");
    }
  });

  test("passes absolute file paths through unchanged", () => {
    const out = redirectPaths("read", WORKTREE, { path: "/etc/hosts" });
    expect(out.path).toBe("/etc/hosts");
  });

  test("does not touch other tools or non-string fields", () => {
    const out = redirectPaths("webfetch", WORKTREE, { url: "src/a.rs" });
    expect(out.url).toBe("src/a.rs");
  });
});

describe("redirectPaths shell workdir (v2 `shell` tool)", () => {
  test("fills a bare shell workdir with the worktree", () => {
    const out = redirectPaths("shell", WORKTREE, { command: "ls" });
    expect(out.workdir).toBe(WORKTREE);
    expect(out.command).toBe("ls");
  });

  test("keeps the v1 `bash` tool name as an alias", () => {
    const out = redirectPaths("bash", WORKTREE, { command: "ls" });
    expect(out.workdir).toBe(WORKTREE);
  });

  test("resolves a relative shell workdir against the worktree", () => {
    const out = redirectPaths("shell", WORKTREE, { command: "ls", workdir: "sub" });
    expect(out.workdir).toBe(`${WORKTREE}/sub`);
  });

  test("keeps an absolute shell workdir and never rewrites the command", () => {
    const out = redirectPaths("shell", WORKTREE, { command: "cd /tmp && ls", workdir: "/tmp" });
    expect(out.workdir).toBe("/tmp");
    expect(out.command).toBe("cd /tmp && ls");
  });

  test("never rewrites shell commands; rewriting lives in the hook", () => {
    const out = redirectPaths("shell", WORKTREE, {
      command: "phasegent issue create --title t --body b",
    });
    expect(out.command).toBe("phasegent issue create --title t --body b");
    expect(out.workdir).toBe(WORKTREE);
  });
});

describe("redirectPaths pass-through", () => {
  test("returns the same object when no worktree is known", () => {
    const args = { path: "src/a.rs" };
    expect(redirectPaths("read", null, args)).toBe(args);
    expect(redirectPaths("shell", "", args)).toBe(args);
  });

  test("returns non-object args untouched", () => {
    expect(redirectPaths("read", WORKTREE, undefined)).toBeUndefined();
    expect(redirectPaths("read", WORKTREE, null)).toBeNull();
  });
});

describe("worktree registry", () => {
  test("maps a worktree only to the exact session that registered it", () => {
    rememberWorktree("session-1", WORKTREE);
    expect(worktreeForSession("session-1")).toBe(WORKTREE);
    // Issue 919 P1: no cross-session fallback. A different or absent session id
    // never borrows session-1's directory — the reported bug where B resolved
    // to A's worktree.
    expect(worktreeForSession("session-2")).toBeNull();
    expect(worktreeForSession(undefined)).toBeNull();
    expect(worktreeForSession(null)).toBeNull();
  });

  test("resetWorktrees clears every exact mapping so sessions pass through", () => {
    rememberWorktree("session-1", WORKTREE);
    resetWorktrees();
    expect(worktreeForSession("session-1")).toBeNull();
  });
});

describe("tool.execute.before hook (command rewriting only)", () => {
  test("rewrites a phasegent command and leaves every other tool alone", async () => {
    const hook = createRedirectHook();
    const shell = {
      tool: "shell",
      sessionID: "session-1",
      agent: "orchestrator",
      input: { command: "phasegent issue get 1" },
    };
    await hook(shell);
    expect(shell.input.command).toBe("PHASEGENT_ROLE=orchestrator phasegent issue get 1");
    expect(shell.input.workdir).toBeUndefined();

    const read = { tool: "read", sessionID: "session-1", input: { path: "src/a.rs" } };
    await hook(read);
    expect(read.input.path).toBe("src/a.rs");
  });

  test("never places a session and never raises a placement error", async () => {
    rememberWorktree("session-1", WORKTREE);
    rememberWorktree("child-1", WORKTREE);
    // The hook takes no context, but a recording spy is passed anyway: if it ever
    // regained a placement path, the move would land in `moves` (and a host
    // without `session.move` would surface a prefixed placement error) instead of
    // this assertion passing on a counter nothing writes to.
    const moves = [];
    const spy = { session: { move: async (input) => moves.push(input) } };
    const hook = createRedirectHook(spy);
    const read = { tool: "read", sessionID: "session-1", input: { path: "src/a.rs" } };
    const child = {
      tool: "read",
      sessionID: "child-1",
      agent: "executor",
      input: { path: "src/a.rs" },
    };
    await expect(hook(read)).resolves.toBeUndefined();
    await expect(hook(child)).resolves.toBeUndefined();
    // No placement state was consulted or produced, and arguments are untouched.
    expect(moves).toEqual([]);
    expect(sessionPlaced("session-1")).toBe(false);
    expect(sessionPlaced("child-1")).toBe(false);
    expect(read.input.path).toBe("src/a.rs");
    expect(child.input.path).toBe("src/a.rs");
  });

  test("ignores calls without input, a command, or a shell tool", async () => {
    const hook = createRedirectHook();
    await expect(hook(undefined)).resolves.toBeUndefined();
    await expect(hook({ tool: "shell", sessionID: "session-1" })).resolves.toBeUndefined();
    const other = { tool: "read", sessionID: "session-1", input: { path: "src/a.rs" } };
    await hook(other);
    expect(other.input.command).toBeUndefined();
  });

  test("refuses issue create for a sub-agent session", async () => {
    const hook = createRedirectHook();
    const event = {
      tool: "shell",
      sessionID: "child-session",
      agent: "executor",
      input: { command: "phasegent issue create --title t --body b" },
    };
    await hook(event);
    expect(event.input.command).toContain("cannot run 'issue create|bind'");
    expect(event.input.command.endsWith("; false")).toBe(true);
    expect(event.input.command).not.toContain("--title");
  });

  test("without a bridge the hook stays rewrite-only", async () => {
    const hook = createRedirectHook(null);
    const event = {
      tool: "shell",
      sessionID: "session-1",
      agent: "explore",
      input: { command: "phasegent issue get 1" },
    };
    await hook(event);
    expect(event.input.command).toBe("PHASEGENT_ROLE=explore phasegent issue get 1");
    expect(event.input.command.startsWith("# phasegent-context:")).toBe(false);
  });
});

describe("managed shell bridge wiring (issue #859 P1)", () => {
  test("redirect hook decorates with a nonce header when a bridge is wired", async () => {
    const bridge = createManagedShellBridge({
      randomUUID: () => "11111111-1111-4111-8111-111111111111",
    });
    const hook = createRedirectHook(bridge);
    const event = {
      tool: "shell",
      sessionID: "ses-1",
      agent: "explore",
      id: "tool-1",
      input: { command: "phasegent issue get 1" },
    };
    await hook(event);
    expect(event.input.command.startsWith("# phasegent-context:11111111-")).toBe(true);
    expect(event.input.command.endsWith("PHASEGENT_ROLE=explore phasegent issue get 1")).toBe(true);
    expect(bridge.size()).toBe(1);
  });

  test("shell hook strips the header and adopts env on the fresh object only", async () => {
    const bridge = createManagedShellBridge({
      randomUUID: () => "22222222-2222-4222-8222-222222222222",
    });
    const redirect = createRedirectHook(bridge);
    const toolEvent = {
      tool: "shell",
      sessionID: "ses-2",
      agent: "executor",
      id: "tool-2",
      input: { command: "phasegent issue get 2" },
    };
    await redirect(toolEvent);
    const shellEvent = { command: toolEvent.input.command, env: { TERM: "xterm" } };
    const shellHook = createShellEnvHook(bridge);
    await shellHook(shellEvent);
    expect(shellEvent.command).toBe("PHASEGENT_ROLE=executor phasegent issue get 2");
    expect(shellEvent.env.PHASEGENT_ROLE).toBe("executor");
    expect(shellEvent.env.PHASEGENT_SESSION_ID).toBe("ses-2");
    expect(process.env.PHASEGENT_SESSION_ID).not.toBe("ses-2");
  });

  test("unknown agent still decorates but clears role in the shell env", async () => {
    const bridge = createManagedShellBridge({
      randomUUID: () => "33333333-3333-4333-8333-333333333333",
    });
    const redirect = createRedirectHook(bridge);
    const toolEvent = {
      tool: "shell",
      sessionID: "ses-9",
      agent: "general",
      id: "tool-9",
      input: { command: "phasegent issue get 9" },
    };
    await redirect(toolEvent);
    expect(toolEvent.input.command.startsWith("# phasegent-context:")).toBe(true);
    const shellEvent = {
      command: toolEvent.input.command,
      env: { PHASEGENT_ROLE: "executor" },
    };
    await createShellEnvHook(bridge)(shellEvent);
    expect("PHASEGENT_ROLE" in shellEvent.env).toBe(false);
    expect(shellEvent.env.PHASEGENT_SESSION_ID).toBe("ses-9");
  });

  test("setup degrades explicitly when the host exposes no shell hook", async () => {
    const warnings = await withWarnings(async () => {
      const registered = [];
      const context = {
        location: { directory: "/repo" },
        session: {
          move: async () => {},
          hook: async (name, callback) => {
            registered.push(name);
            return { dispose: async () => {} };
          },
        },
        tool: {
          hook: async (name, callback) => {
            registered.push(name);
            return { dispose: async () => {} };
          },
        },
        skill: { transform: async () => ({ dispose: async () => {} }) },
        agent: { transform: async () => ({ dispose: async () => {} }) },
      };
      const cleanup = await PhasegentWorktreePlugin.setup(context);
      expect(registered).toContain("execute.before");
      expect(registered).not.toContain("create.before");
      expect(typeof cleanup).toBe("function");
      await cleanup();
    });
    expect(warnings.join("\n")).toContain("no shell hook");
  });

  test("setup registers shell and after hooks when the host supports them", async () => {
    const registered = [];
    const context = {
      location: { directory: "/repo" },
      session: {
        move: async () => {},
        hook: async (name, callback) => {
          registered.push(name);
          return { dispose: async () => {} };
        },
      },
      tool: {
        hook: async (name, callback) => {
          registered.push(name);
          return { dispose: async () => {} };
        },
      },
      shell: {
        hook: async (name, callback) => {
          registered.push(name);
          return { dispose: async () => {} };
        },
      },
      skill: { transform: async () => ({ dispose: async () => {} }) },
      agent: { transform: async () => ({ dispose: async () => {} }) },
    };
    const cleanup = await PhasegentWorktreePlugin.setup(context);
    expect(registered).toContain("execute.before");
    expect(registered).toContain("execute.after");
    expect(registered).toContain("create.before");
    expect(typeof isShellHookSupported(context)).toBe("boolean");
    expect(isShellHookSupported(context)).toBe(true);
    expect(typeof createToolAfterHook).toBe("function");
    await cleanup();
  });
});

describe("session prompt hook: worktree placement (issue 37)", () => {
  let savedNoDiscover;
  beforeEach(() => {
    savedNoDiscover = process.env.PHASEGENT_WORKTREE_NO_DISCOVER;
    delete process.env.PHASEGENT_WORKTREE_NO_DISCOVER;
  });
  afterEach(() => {
    restoreNoDiscover(savedNoDiscover);
  });

  test("moves a child into the parent's registered worktree on its first prompt", async () => {
    rememberWorktree("parent-1", WORKTREE);
    // A stale global fallback must not win over the parent's own target.
    rememberWorktree("other-session", "/wt/other");
    const moves = [];
    const sessions = {
      "child-1": { parentID: "parent-1", location: { directory: "/repo" } },
      "parent-1": { parentID: null, location: { directory: "/wt/host-parent" } },
    };
    const prompt = createPromptHook(hostContext(sessions, moves), noCliDeps);

    // The prompt is not cancelled: it has run no tool in the old directory, so
    // the move is admitted and lands at the runner's next step boundary, before
    // the child's first tool call.
    await expect(prompt(promptEvent("child-1"))).resolves.toBeUndefined();
    expect(moves).toEqual([{ sessionID: "child-1", directory: WORKTREE }]);

    // A second prompt before the landing is still admitted, never cancelled.
    await expect(prompt(promptEvent("child-1"))).resolves.toBeUndefined();
    expect(moves).toHaveLength(1);

    // The host applied the move: the child is placed and stays that way.
    sessions["child-1"] = { parentID: "parent-1", location: { directory: WORKTREE } };
    await expect(prompt(promptEvent("child-1"))).resolves.toBeUndefined();
    expect(sessionPlaced("child-1")).toBe(true);
    expect(moves).toHaveLength(1);
  });

  test("falls back to the parent's host-reported directory", async () => {
    // No registry entry for the parent, and a stale global fallback in place.
    rememberWorktree("other-session", "/wt/other");
    const moves = [];
    const sessions = {
      "child-1": { parentID: "parent-1", location: { directory: "/repo" } },
      "parent-1": { parentID: null, location: { directory: WORKTREE } },
    };
    const prompt = createPromptHook(hostContext(sessions, moves), noCliDeps);

    await expect(prompt(promptEvent("child-1"))).resolves.toBeUndefined();
    expect(moves).toEqual([{ sessionID: "child-1", directory: WORKTREE }]);
  });

  test("leaves a child that already sits in the parent's directory untouched", async () => {
    const moves = [];
    const sessions = {
      "child-1": { parentID: "parent-1", location: { directory: WORKTREE } },
      "parent-1": { parentID: null, location: { directory: WORKTREE } },
    };
    const prompt = createPromptHook(hostContext(sessions, moves), noCliDeps);

    await expect(prompt(promptEvent("child-1"))).resolves.toBeUndefined();
    // The child already runs in the parent's directory: placed without a move.
    expect(moves).toEqual([]);
    expect(sessionPlaced("child-1")).toBe(true);
  });

  test("keeps a child in place when the parent has no worktree", async () => {
    rememberWorktree("other-session", "/wt/other");
    const moves = [];
    const sessions = {
      "child-1": { parentID: "parent-1", location: { directory: "/repo" } },
      "parent-1": { parentID: null, location: { directory: "/repo" } },
    };
    const prompt = createPromptHook(hostContext(sessions, moves), noCliDeps);

    await expect(prompt(promptEvent("child-1"))).resolves.toBeUndefined();
    expect(moves).toEqual([]);
  });

  test("ignores a session with no reported parent and an empty prompt event", async () => {
    const moves = [];
    const sessions = {
      "session-1": { parentID: null, location: { directory: "/repo" } },
    };
    const prompt = createPromptHook(hostContext(sessions, moves), noCliDeps);

    await expect(prompt(promptEvent("session-1"))).resolves.toBeUndefined();
    await expect(prompt(undefined)).resolves.toBeUndefined();
    await expect(prompt({})).resolves.toBeUndefined();
    expect(moves).toEqual([]);
  });

  test("fails the prompt when the host rejects the move", async () => {
    rememberWorktree("parent-1", WORKTREE);
    const sessions = {
      "child-1": { parentID: "parent-1", location: { directory: "/repo" } },
    };
    const context = {
      location: { directory: "/repo" },
      session: {
        get: async ({ sessionID }) => sessions[sessionID],
        move: async () => {
          throw new Error("destination unavailable");
        },
      },
    };
    const prompt = createPromptHook(context, noCliDeps);
    await expect(prompt(promptEvent("child-1"))).rejects.toThrow(
      /session placement failed.*destination unavailable/,
    );
  });

  test("never places at tool time and keeps no placement fallback there", async () => {
    rememberWorktree("parent-1", WORKTREE);
    // A stale registry entry for a sibling must not become the child's target
    // through the global fallback either.
    rememberWorktree("other-session", "/wt/other");
    const moves = [];
    const sessions = {
      "child-1": { parentID: "parent-1", location: { directory: "/repo" } },
      "parent-1": { parentID: null, location: { directory: WORKTREE } },
    };
    const context = hostContext(sessions, moves);
    const hook = createRedirectHook();

    const event = { tool: "read", sessionID: "child-1", agent: "executor", input: { path: "a.rs" } };
    // The tool hook never places a child and never raises a placement error.
    await expect(hook(event)).resolves.toBeUndefined();
    expect(moves).toEqual([]);
    expect(sessionPlaced("child-1")).toBe(false);

    // And the child still refuses the sibling's registry fallback: it takes its
    // parent's directory or nothing, so the stale entry is never used.
    const prompt = createPromptHook(context, noCliDeps);
    await expect(prompt(promptEvent("child-1"))).resolves.toBeUndefined();
    expect(moves).toEqual([{ sessionID: "child-1", directory: WORKTREE }]);
  });

  test("keeps the exact registry-only path when host discovery is disabled", async () => {
    process.env.PHASEGENT_WORKTREE_NO_DISCOVER = "1";
    rememberWorktree("child-1", WORKTREE);
    // A different session's registration must never be borrowed.
    rememberWorktree("other-session", "/wt/other");
    const moves = [];
    let lookups = 0;
    const context = {
      location: { directory: "/repo" },
      session: {
        move: async (input) => moves.push(input),
        get: async ({ sessionID }) => {
          lookups += 1;
          return { parentID: null, location: { directory: `/repo/${sessionID}` } };
        },
      },
    };
    const prompt = createPromptHook(context, noCliDeps);

    // Issue 919 P1: disabled discovery is inert — the exact in-memory lookup
    // returns the session's own worktree, but no host read and no auto-move
    // happen, because the current location cannot be verified here.
    await expect(prompt(promptEvent("child-1"))).resolves.toBeUndefined();
    expect(moves).toEqual([]);
    expect(lookups).toBe(0);
    // An unknown session is not handed another session's worktree.
    await expect(prompt(promptEvent("unknown-session"))).resolves.toBeUndefined();
    expect(moves).toEqual([]);
    expect(lookups).toBe(0);
  });

  test("moves a registered session and leaves later prompts a no-op", async () => {
    rememberWorktree("session-1", WORKTREE);
    const moves = [];
    const sessions = {
      "session-1": { parentID: null, location: { directory: "/repo" } },
    };
    const prompt = createPromptHook(hostContext(sessions, moves), noCliDeps);

    // The move is admitted and the prompt continues: no pending error, and the
    // runner applies the placement at its next step boundary.
    await expect(prompt(promptEvent("session-1"))).resolves.toBeUndefined();
    expect(moves).toEqual([{ sessionID: "session-1", directory: WORKTREE }]);
    expect(sessionPlaced("session-1")).toBe(true);

    // Already settled: a second prompt issues no further move, whether or not
    // the host record has caught up yet.
    await expect(prompt(promptEvent("session-1"))).resolves.toBeUndefined();
    expect(moves).toHaveLength(1);
  });

  test("moves a session already sitting in the target without a move", async () => {
    rememberWorktree("session-1", WORKTREE);
    const moves = [];
    const sessions = {
      "session-1": { parentID: null, location: { directory: WORKTREE } },
    };
    // The plugin already runs in the worktree, so no move is needed.
    const context = { ...hostContext(sessions, moves), location: { directory: WORKTREE } };
    const prompt = createPromptHook(context, noCliDeps);

    await expect(prompt(promptEvent("session-1"))).resolves.toBeUndefined();
    expect(moves).toEqual([]);
    expect(sessionPlaced("session-1")).toBe(true);
  });

  test("fails the prompt when the host exposes no session.move", async () => {
    rememberWorktree("session-1", WORKTREE);
    const sessions = {
      "session-1": { parentID: null, location: { directory: "/repo" } },
    };
    const context = { location: { directory: "/repo" }, session: { get: async () => sessions["session-1"] } };
    const prompt = createPromptHook(context, noCliDeps);

    // Fail closed: the prompt does not run unplaced in the old checkout.
    await expect(prompt(promptEvent("session-1"))).rejects.toThrow(
      /session placement unavailable.*session\.move/,
    );
  });

  test("retries a failed move on the next prompt", async () => {
    rememberWorktree("session-1", WORKTREE);
    let attempts = 0;
    const moves = [];
    const context = {
      location: { directory: "/repo" },
      session: {
        get: async () => ({ parentID: null, location: { directory: "/repo" } }),
        move: async (input) => {
          attempts += 1;
          if (attempts === 1) throw new Error("runner not ready");
          moves.push(input);
        },
      },
    };
    const prompt = createPromptHook(context, noCliDeps);

    // The rejected move fails this prompt and releases the attempt.
    await expect(prompt(promptEvent("session-1"))).rejects.toThrow(
      /session placement failed.*runner not ready/,
    );
    expect(sessionPlaced("session-1")).toBe(false);

    // The next prompt retries, and the admitted move settles the placement.
    await expect(prompt(promptEvent("session-1"))).resolves.toBeUndefined();
    expect(attempts).toBe(2);
    expect(moves).toEqual([{ sessionID: "session-1", directory: WORKTREE }]);
    expect(sessionPlaced("session-1")).toBe(true);
  });

  test("resetWorktrees clears the settled placement so a fresh move is allowed", async () => {
    rememberWorktree("session-1", WORKTREE);
    const moves = [];
    const context = {
      location: { directory: "/repo" },
      session: {
        get: async () => ({ parentID: null, location: { directory: "/repo" } }),
        move: async (input) => moves.push(input),
      },
    };
    const prompt = createPromptHook(context, noCliDeps);
    await expect(prompt(promptEvent("session-1"))).resolves.toBeUndefined();
    expect(moves).toHaveLength(1);

    resetWorktrees();
    expect(sessionPlaced("session-1")).toBe(false);
    expect(worktreeForSession("session-1")).toBeNull();

    rememberWorktree("session-1", WORKTREE);
    await expect(prompt(promptEvent("session-1"))).resolves.toBeUndefined();
    expect(moves).toHaveLength(2);
  });

  test("resumes an existing child whose parent moved to another worktree", async () => {
    rememberWorktree("parent-1", WORKTREE);
    const moves = [];
    // A resumed child still sits in the checkout it was started in, while the
    // parent's registered target is a different worktree.
    const sessions = {
      "child-1": { parentID: "parent-1", location: { directory: "/repo" } },
    };
    const prompt = createPromptHook(hostContext(sessions, moves), noCliDeps);
    await expect(prompt(promptEvent("child-1"))).resolves.toBeUndefined();
    expect(moves).toEqual([{ sessionID: "child-1", directory: WORKTREE }]);

    // The parent moves again: the child follows on its next prompt.
    const next = "/repo/.worktrees/issue-567";
    resetWorktrees();
    rememberWorktree("parent-1", next);
    moves.length = 0;
    await expect(prompt(promptEvent("child-1"))).resolves.toBeUndefined();
    expect(moves).toEqual([{ sessionID: "child-1", directory: next }]);
  });

  test("leaves a prompt event with no session id untouched", async () => {
    const moves = [];
    const context = hostContext({}, moves);
    const prompt = createPromptHook(context, noCliDeps);

    await expect(prompt({ messageID: "msg-1", prompt: { text: "hi" } })).resolves.toBeUndefined();
    expect(moves).toEqual([]);
    expect(await ensureSessionWorktree(context, null)).toBeNull();
  });
});

describe("worktree discovery and inheritance helpers (issue #567)", () => {
  let savedNoDiscover;
  beforeEach(() => {
    savedNoDiscover = process.env.PHASEGENT_WORKTREE_NO_DISCOVER;
    delete process.env.PHASEGENT_WORKTREE_NO_DISCOVER;
  });
  afterEach(() => {
    restoreNoDiscover(savedNoDiscover);
  });

  // `discover`/`readBinding` are stubbed so a missed inheritance fails loudly
  // instead of spawning the phasegent CLI; `readSessionInfo` stays real and
  // reads the fake host. The lazy path never creates a worktree on its own
  // (issue 616), so there is no acquire seam to stub.
  const noCliDeps = {
    discover: async () => null,
    readBinding: async () => 567,
    readLeaseHistory: async () => [],
  };

  test("no session without a reusable lease creates a worktree lazily", async () => {
    // Issue 616: the lazy path never creates a directory. A session with no
    // reusable lease stays in the current checkout and gets the explicit
    // isolation guidance once, and the guidance is not repeated per prompt.
    const moves = [];
    const warnings = [];
    const original = console.warn;
    console.warn = (message) => warnings.push(String(message));
    const deps = {
      readSessionInfo: async () => null,
      discover: async () => null,
      readBinding: async () => 567,
      readLeaseHistory: async () => [],
    };
    const context = {
      location: { directory: "/repo" },
      session: { move: async (input) => moves.push(input) },
    };
    try {
      expect(await ensureSessionWorktree(context, "parent-1", deps)).toBeNull();
      expect(await ensureSessionWorktree(context, "parent-1", deps)).toBeNull();
    } finally {
      console.warn = original;
    }
    expect(moves).toEqual([]);
    expect(sessionPlaced("parent-1")).toBe(false);
    expect(warnings).toHaveLength(1);
    expect(warnings[0]).toContain("issue 567");
    expect(warnings[0]).toContain("isolation is opt-in");
    expect(warnings[0]).toContain("--isolate");
  });

  test("a discovered lease is reused, and a closed issue is refused", async () => {
    const moves = [];
    // The host reports the session's current directory, so the decided move has
    // a verified source (issue 919 P1: a decided placement never guesses it).
    const context = {
      location: { directory: "/repo" },
      session: {
        get: async () => ({ parentID: null, location: { directory: "/repo" } }),
        move: async (input) => moves.push(input),
      },
    };
    expect(
      await ensureSessionWorktree(context, "orch-1", {
        readSessionInfo: async () => null,
        discover: async () => WORKTREE,
      }),
    ).toBe(WORKTREE);
    expect(moves).toEqual([{ sessionID: "orch-1", directory: WORKTREE }]);

    // A lease `issue close` converged keeps its release reason, so the lazy path
    // refuses to rebuild a worktree for that issue.
    const warnings = [];
    const original = console.warn;
    console.warn = (message) => warnings.push(String(message));
    try {
      expect(
        await ensureSessionWorktree(context, "orch-2", {
          readSessionInfo: async () => null,
          discover: async () => null,
          readBinding: async () => 567,
          readLeaseHistory: async () => [{ status: "released", release_reason: "issue closed" }],
        }),
      ).toBeNull();
    } finally {
      console.warn = original;
    }
    expect(moves).toHaveLength(1);
    expect(warnings.join("\n")).toContain("issue 567 is closed");
  });

  test("caches the host session lookup and clears it with the registry", async () => {
    let calls = 0;
    const context = {
      session: {
        get: async ({ sessionID }) => {
          calls += 1;
          return { parentID: null, location: { directory: `/repo/${sessionID}` } };
        },
      },
    };
    expect(await readSessionInfo(context, "s1")).toEqual({
      parentID: null,
      directory: "/repo/s1",
    });
    expect(await readSessionInfo(context, "s1")).toEqual({
      parentID: null,
      directory: "/repo/s1",
    });
    expect(calls).toBe(1);

    resetWorktrees();
    await readSessionInfo(context, "s1");
    expect(calls).toBe(2);
  });

  test("retries a failed session lookup and degrades without one", async () => {
    let calls = 0;
    const flaky = {
      session: {
        get: async () => {
          calls += 1;
          if (calls === 1) throw new Error("session store unavailable");
          return { parentID: "parent-1", location: { directory: WORKTREE } };
        },
      },
    };
    expect(await readSessionInfo(flaky, "s1")).toBeNull();
    expect(await readSessionInfo(flaky, "s1")).toEqual({
      parentID: "parent-1",
      directory: WORKTREE,
    });
    expect(calls).toBe(2);

    resetWorktrees();
    expect(await readSessionInfo(undefined, "s1")).toBeNull();
    expect(await readSessionInfo({}, "s2")).toBeNull();
    const odd = { session: { get: async () => "not a session" } };
    expect(await readSessionInfo(odd, "s3")).toBeNull();
    // A non-object result stays uncached, so a usable answer still lands later.
    odd.session.get = async () => ({ parentID: null, location: { directory: "/repo" } });
    expect(await readSessionInfo(odd, "s3")).toEqual({ parentID: null, directory: "/repo" });
  });

  test("prefers the parent's registered target over the host-reported directory", async () => {
    const context = {
      session: { get: async () => ({ parentID: null, location: { directory: "/wt/host" } }) },
    };
    rememberWorktree("parent-1", WORKTREE);
    expect(await inheritedWorktree(context, "parent-1")).toBe(WORKTREE);
    expect(await inheritedWorktree(context, "parent-2")).toBe("/wt/host");
    expect(await inheritedWorktree(context, null)).toBeNull();
  });
});

// ---------------------------------------------------------------------------
// Host-authoritative reconciliation (issue 747 P2)
//
// `issue close` returns the closing session to the repository's main checkout
// through the OpenCode API before it removes the worktree, so a registry entry
// — and the shared fallback built from it — can name a directory that is already
// gone. The host is the authority that outranks both, and every case here is a
// direction the plugin must not reverse.
// ---------------------------------------------------------------------------

describe("host-authoritative reconciliation (issue 747 P2)", () => {
  let savedNoDiscover;
  beforeEach(() => {
    savedNoDiscover = process.env.PHASEGENT_WORKTREE_NO_DISCOVER;
    delete process.env.PHASEGENT_WORKTREE_NO_DISCOVER;
  });
  afterEach(() => {
    restoreNoDiscover(savedNoDiscover);
  });

  const MAIN = "/repo";

  test("an authoritative read is never served from the session cache", async () => {
    let calls = 0;
    let directory = MAIN;
    const context = {
      session: {
        get: async () => {
          calls += 1;
          return { parentID: null, location: { directory } };
        },
      },
    };
    expect(await readSessionInfo(context, "s1")).toEqual({ parentID: null, directory: MAIN });
    directory = "/repo/main";
    // The ordinary cached lookup keeps its answer: the cache exists so an
    // inheritance read does not reach the host on every turn.
    expect(await readSessionInfo(context, "s1")).toEqual({ parentID: null, directory: MAIN });
    expect(calls).toBe(1);
    // The reconciliation is answered by the host, and refreshes the cache.
    expect(await readAuthoritativeDirectory(context, "s1")).toBe("/repo/main");
    expect(await readSessionInfo(context, "s1")).toEqual({
      parentID: null,
      directory: "/repo/main",
    });
    expect(calls).toBe(2);
  });

  test("an unsettled session is left alone and the first placement still happens", async () => {
    // A session that has not settled yet legitimately sits in the main
    // checkout: that difference is the placement, not a relocation, so it
    // must not cancel the move.
    rememberWorktree("session-1", WORKTREE);
    let lookups = 0;
    const moves = [];
    const context = {
      location: { directory: MAIN },
      session: {
        get: async () => {
          lookups += 1;
          return { parentID: null, location: { directory: MAIN } };
        },
        move: async (input) => moves.push(input),
      },
    };
    expect(await reconcileSessionLocation(context, "session-1")).toBe(true);
    expect(lookups).toBe(0);
    expect(worktreeForSession("session-1")).toBe(WORKTREE);

    expect(await ensureSessionWorktree(context, "session-1", noCliDeps)).toBe(WORKTREE);
    expect(moves).toEqual([{ sessionID: "session-1", directory: WORKTREE }]);
    expect(sessionPlaced("session-1")).toBe(true);
  });

  test("never reverses a session the close returned to the main checkout", async () => {
    rememberWorktree("session-1", WORKTREE);
    const moves = [];
    let directory = MAIN;
    const context = {
      location: { directory: MAIN },
      session: {
        get: async () => ({ parentID: null, location: { directory } }),
        move: async ({ directory: target }) => {
          moves.push(target);
          directory = target;
        },
      },
    };
    const prompt = createPromptHook(context, noCliDeps);
    await prompt(promptEvent("session-1"));
    expect(moves).toEqual([WORKTREE]);
    expect(sessionPlaced("session-1")).toBe(true);

    // `issue close` asks the API to return the session to the main checkout,
    // and the API is what decided — not this registry.
    directory = MAIN;
    await prompt(promptEvent("session-1"));
    expect(moves).toEqual([WORKTREE]);
    expect(sessionPlaced("session-1")).toBe(false);
    // The stale target is gone for this session and for the shared fallback
    // that would otherwise hand it to the next sub-agent.
    expect(worktreeForSession("session-1")).toBeNull();
    expect(worktreeForSession("unknown-session")).toBeNull();
  });

  test("an unreadable host never revives a worktree the close may have removed", async () => {
    rememberWorktree("session-1", WORKTREE);
    let directory = MAIN;
    let unavailable = false;
    const moves = [];
    const context = {
      location: { directory: MAIN },
      session: {
        get: async () => {
          if (unavailable) throw new Error("session store unavailable");
          return { parentID: null, location: { directory } };
        },
        move: async ({ directory: target }) => {
          moves.push(target);
          directory = target;
        },
      },
    };
    const prompt = createPromptHook(context, noCliDeps);
    await prompt(promptEvent("session-1"));
    expect(moves).toEqual([WORKTREE]);
    expect(sessionPlaced("session-1")).toBe(true);

    // "I cannot tell you where this session is" is not "it is still in the
    // worktree": the directory may already be gone, so the cached target is
    // dropped instead of being re-entered.
    unavailable = true;
    await prompt(promptEvent("session-1"));
    expect(moves).toHaveLength(1);
    expect(sessionPlaced("session-1")).toBe(false);
    expect(worktreeForSession("session-1")).toBeNull();
    expect(worktreeForSession("unknown-session")).toBeNull();
  });

  test("a child follows the parent to where it actually is, not the cached worktree", async () => {
    const moves = [];
    const sessions = {
      "parent-1": { parentID: null, location: { directory: WORKTREE } },
      "child-1": { parentID: "parent-1", location: { directory: WORKTREE } },
    };
    const context = hostContext(sessions, moves);
    const prompt = createPromptHook(context, noCliDeps);
    rememberWorktree("parent-1", WORKTREE);
    await prompt(promptEvent("parent-1"));
    expect(sessionPlaced("parent-1")).toBe(true);

    // The close returns the parent to the main checkout; the child's cached
    // worktree is the directory that is about to disappear.
    sessions["parent-1"].location.directory = MAIN;
    sessions["child-2"] = { parentID: "parent-1", location: { directory: WORKTREE } };
    moves.length = 0;
    await prompt(promptEvent("child-2"));

    expect(moves).toEqual([{ sessionID: "child-2", directory: MAIN }]);
    // The parent's settled registration is gone for its own id, and no other
    // session borrows it (issue 919 P1: no cross-session fallback).
    expect(sessionPlaced("parent-1")).toBe(false);
    expect(worktreeForSession("parent-1")).not.toBe(WORKTREE);
    expect(worktreeForSession("unknown-session")).toBeNull();
  });

  test("never hands another session's worktree to a new sub-agent", async () => {
    // Issue 919 P1: with no cross-session fallback, a new session with no own
    // registration and no parent never borrows another session's worktree, no
    // matter the other session's lease state.
    rememberWorktree("other-session", WORKTREE);
    const moves = [];
    const context = {
      location: { directory: MAIN },
      session: { get: async () => null, move: async (input) => moves.push(input) },
    };
    const warnings = await withWarnings(async () => {
      expect(
        await ensureSessionWorktree(context, "sub-1", {
          readSessionInfo: async () => null,
          discover: async () => null,
          readBinding: async () => 747,
          readLeaseHistory: async () => [
            { status: "retained", release_reason: "issue closed: ses_a" },
          ],
        }),
      ).toBeNull();
    });
    expect(moves).toEqual([]);
    expect(warnings.join("\n")).toContain("issue 747 is closed");
    expect(warnings.join("\n")).toContain("refusing to acquire a worktree");
  });

  test("an open issue still does not borrow another session's worktree", async () => {
    rememberWorktree("other-session", WORKTREE);
    const moves = [];
    const context = {
      location: { directory: MAIN },
      session: { get: async () => null, move: async (input) => moves.push(input) },
    };
    expect(
      await ensureSessionWorktree(context, "sub-2", {
        readSessionInfo: async () => null,
        discover: async () => null,
        readBinding: async () => 747,
        readLeaseHistory: async () => [{ status: "active", release_reason: null }],
      }),
    ).toBeNull();
    expect(moves).toEqual([]);
  });

  test("a moved session re-registers cleanly on its next issue's lease", async () => {
    // Reconciliation drops the stale target; the ordinary discovery path is
    // untouched, so a later, still-open issue re-registers from its own lease.
    rememberWorktree("session-1", WORKTREE);
    let directory = MAIN;
    const context = {
      location: { directory: MAIN },
      session: {
        get: async () => ({ parentID: null, location: { directory } }),
        move: async ({ directory: target }) => {
          directory = target;
        },
      },
    };
    const prompt = createPromptHook(context, noCliDeps);
    await prompt(promptEvent("session-1"));
    directory = MAIN;
    await prompt(promptEvent("session-1"));
    expect(worktreeForSession("session-1")).toBeNull();

    const next = "/repo/.worktrees/issue-748";
    expect(
      await ensureSessionWorktree(context, "session-1", {
        ...noCliDeps,
        readSessionInfo: async () => null,
        discover: async () => next,
        readBinding: async () => null,
      }),
    ).toBe(next);
    expect(directory).toBe(next);
  });

  test("reads nothing from the host while host discovery is disabled", async () => {
    // NO_DISCOVER keeps the adapter inert beyond the in-memory registry: no
    // reconciliation read either, or the inert mode would reach the host.
    process.env.PHASEGENT_WORKTREE_NO_DISCOVER = "1";
    try {
      rememberWorktree("session-1", WORKTREE);
      const moves = [];
      let lookups = 0;
      const context = {
        location: { directory: MAIN },
        session: {
          get: async () => {
            lookups += 1;
            return { parentID: null, location: { directory: MAIN } };
          },
          move: async (input) => moves.push(input),
        },
      };
      expect(await ensureSessionWorktree(context, "session-1", noCliDeps)).toBe(WORKTREE);
      // The inert path is local-only and never auto-moves: the exact lookup
      // returns the worktree, but with no host read the current location cannot
      // be verified, so no move is issued.
      expect(await ensureSessionWorktree(context, "session-1", noCliDeps)).toBe(WORKTREE);
      expect(lookups).toBe(0);
      expect(moves).toHaveLength(0);
    } finally {
      restoreNoDiscover(savedNoDiscover);
    }
  });
});

// ---------------------------------------------------------------------------
// Session-scoped placement and bounded diagnostics (issue 919 P1)
//
// The registry is exact per session; the host's fresh own-session directory is
// the only authority on whether a decided move is needed; and each placement
// decision emits one bounded diagnostic line. No cross-session fallback.
// ---------------------------------------------------------------------------

describe("session-scoped placement (issue 919 P1)", () => {
  let savedNoDiscover;
  beforeEach(() => {
    savedNoDiscover = process.env.PHASEGENT_WORKTREE_NO_DISCOVER;
    delete process.env.PHASEGENT_WORKTREE_NO_DISCOVER;
  });
  afterEach(() => {
    restoreNoDiscover(savedNoDiscover);
  });

  // A host that records every `get` and `move`, and reports the directory from
  // a mutable map, so a test can interleave sessions without a real host.
  function recordingHost(sessions) {
    const gets = [];
    const moves = [];
    return {
      gets,
      moves,
      context: {
        location: { directory: "/repo" },
        session: {
          get: async ({ sessionID }) => {
            gets.push(sessionID);
            return sessions[sessionID];
          },
          move: async ({ sessionID, directory }) => {
            moves.push({ sessionID, directory });
            sessions[sessionID] = { ...sessions[sessionID], location: { directory } };
          },
        },
      },
    };
  }

  test("interleaves two independent roots without cross-talk", async () => {
    const rootA = "/audit/repo-A/worktree";
    const rootB = "/audit/repo-B/worktree";
    rememberWorktree("root-A", rootA);
    rememberWorktree("root-B", rootB);
    const host = recordingHost({
      "root-A": { parentID: null, location: { directory: "/audit/repo-A" } },
      "root-B": { parentID: null, location: { directory: "/audit/repo-B" } },
      "root-C": { parentID: null, location: { directory: "/audit/repo-C" } },
    });
    await ensureSessionWorktree(host.context, "root-A", noCliDeps);
    await ensureSessionWorktree(host.context, "root-B", noCliDeps);
    // An unrelated root is not moved anywhere, let alone into A's or B's tree.
    await ensureSessionWorktree(host.context, "root-C", {
      readSessionInfo: async () => null,
      discover: async () => null,
      readBinding: async () => null,
    });
    expect(host.moves).toEqual([
      { sessionID: "root-A", directory: rootA },
      { sessionID: "root-B", directory: rootB },
    ]);
    expect(worktreeForSession("root-C")).toBeNull();
  });

  test("never returns session A's worktree for session B", async () => {
    rememberWorktree("A", "/audit/repo-A/worktree");
    // The reported bug: B, which never registered, resolved to A's directory.
    expect(worktreeForSession("B")).toBeNull();
    // And through the placement decision B stays put rather than entering A's.
    const host = recordingHost({ B: { parentID: null, location: { directory: "/repo" } } });
    expect(
      await ensureSessionWorktree(host.context, "B", {
        readSessionInfo: async () => null,
        discover: async () => null,
        readBinding: async () => null,
      }),
    ).toBeNull();
    expect(host.moves).toEqual([]);
  });

  test("an unknown root and an unavailable parent never move into A's worktree", async () => {
    rememberWorktree("A", WORKTREE);
    const host = recordingHost({
      root: { parentID: null, location: { directory: "/repo" } },
      orphan: { parentID: "missing-parent", location: { directory: "/repo" } },
    });
    expect(
      await ensureSessionWorktree(host.context, "root", {
        readSessionInfo: async () => null,
        discover: async () => null,
        readBinding: async () => null,
      }),
    ).toBeNull();
    expect(
      await ensureSessionWorktree(host.context, "orphan", {
        readSessionInfo: async () => ({ parentID: "missing-parent", directory: "/repo" }),
        discover: async () => null,
        readBinding: async () => null,
      }),
    ).toBeNull();
    expect(host.moves).toEqual([]);
  });

  test("a fresh own-directory read equal to the target yields zero moves despite stale context", async () => {
    rememberWorktree("session-1", WORKTREE);
    // The plugin context still points at the main checkout; the host is the
    // authority and reports the session already in the worktree.
    const host = recordingHost({
      "session-1": { parentID: null, location: { directory: WORKTREE } },
    });
    const context = { ...host.context, location: { directory: "/repo" } };
    expect(await reconcileSessionLocation(context, "session-1")).toBe(true);
    await expect(ensureSessionWorktree(context, "session-1", noCliDeps)).resolves.toBe(WORKTREE);
    expect(host.moves).toEqual([]);
    expect(sessionPlaced("session-1")).toBe(true);
  });

  test("explicit parent inheritance still works and an unsettled parent keeps its admitted target", async () => {
    rememberWorktree("parent-1", WORKTREE);
    // Parent move admitted but the host has not applied it yet: it still
    // reports the old directory. The child must follow the admitted target, not
    // the lagging host answer.
    const host = recordingHost({
      "child-1": { parentID: "parent-1", location: { directory: "/repo" } },
      "parent-1": { parentID: null, location: { directory: "/repo" } },
    });
    await ensureSessionWorktree(host.context, "child-1", noCliDeps);
    expect(host.moves).toEqual([{ sessionID: "child-1", directory: WORKTREE }]);
  });

  test("an externally returned parent is followed, and descendants never re-enter the old worktree", async () => {
    rememberWorktree("parent-1", WORKTREE);
    const sessions = {
      "parent-1": { parentID: null, location: { directory: WORKTREE } },
    };
    const host = recordingHost(sessions);
    const prompt = createPromptHook(host.context, noCliDeps);
    await prompt(promptEvent("parent-1"));
    // The parent already sits in the worktree: settled + confirmed, no move.
    expect(host.moves).toEqual([]);
    expect(sessionPlaced("parent-1")).toBe(true);

    // `issue close` returns the parent to the main checkout.
    sessions["parent-1"] = { parentID: null, location: { directory: "/repo" } };
    sessions["child-2"] = { parentID: "parent-1", location: { directory: WORKTREE } };
    await prompt(promptEvent("child-2"));
    expect(host.moves).toEqual([{ sessionID: "child-2", directory: "/repo" }]);
    expect(worktreeForSession("parent-1")).not.toBe(WORKTREE);
    expect(worktreeForSession("child-2")).toBe("/repo");
  });

  test("concurrent prompts issue only one move; a rejection permits a safe retry", async () => {
    rememberWorktree("session-1", WORKTREE);
    let attempts = 0;
    const moves = [];
    const sessions = { "session-1": { parentID: null, location: { directory: "/repo" } } };
    const context = {
      location: { directory: "/repo" },
      session: {
        get: async () => sessions["session-1"],
        move: async ({ directory }) => {
          attempts += 1;
          if (attempts === 1) throw new Error("runner not ready");
          moves.push(directory);
          sessions["session-1"] = { parentID: null, location: { directory } };
        },
      },
    };
    const prompt = createPromptHook(context, noCliDeps);
    // Two concurrent prompts join the same in-flight attempt: only one move is
    // requested, and the rejection fails both without settling.
    const first = prompt(promptEvent("session-1")).then(
      () => null,
      (error) => error,
    );
    const second = prompt(promptEvent("session-1")).then(
      () => null,
      (error) => error,
    );
    const [firstError, secondError] = await Promise.all([first, second]);
    expect(firstError).toBeInstanceOf(Error);
    expect(String(firstError.message)).toContain("session placement failed");
    expect(secondError).toBeInstanceOf(Error);
    expect(attempts).toBe(1);
    expect(moves).toEqual([]);
    expect(sessionPlaced("session-1")).toBe(false);

    // A later prompt retries and the admitted move settles.
    await expect(prompt(promptEvent("session-1"))).resolves.toBeUndefined();
    expect(attempts).toBe(2);
    expect(moves).toEqual([WORKTREE]);
    expect(sessionPlaced("session-1")).toBe(true);
  });

  test("an unavailable decided location fails the prompt instead of running in the old directory", async () => {
    rememberWorktree("session-1", WORKTREE);
    // The host exposes move but no get: the current directory cannot be
    // verified, so the decided placement must fail closed.
    const moves = [];
    const context = {
      location: { directory: "/repo" },
      session: { move: async (input) => moves.push(input) },
    };
    await expect(ensureSessionWorktree(context, "session-1", noCliDeps)).rejects.toThrow(
      /session placement unverified/,
    );
    expect(moves).toEqual([]);
    expect(sessionPlaced("session-1")).toBe(false);
  });

  test("after a state reset a stale context still cannot author a redundant relocation", async () => {
    rememberWorktree("session-1", WORKTREE);
    const sessions = { "session-1": { parentID: null, location: { directory: WORKTREE } } };
    const host = recordingHost(sessions);
    // Module state is lost (a host reload) while the session already sits in
    // its target; the fresh read confirms it and no move is issued.
    resetWorktrees();
    rememberWorktree("session-1", WORKTREE);
    const context = { ...host.context, location: { directory: "/repo" } };
    await ensureSessionWorktree(context, "session-1", noCliDeps);
    expect(host.moves).toEqual([]);
  });

  test("emits bounded diagnostics only, with a single line per decision and no settled chatter", async () => {
    const warnings = await withWarnings(async () => {
      rememberWorktree("session-1", WORKTREE);
      const sessions = { "session-1": { parentID: null, location: { directory: "/repo" } } };
      const host = recordingHost(sessions);
      await ensureSessionWorktree(host.context, "session-1", noCliDeps);
      // A second, unchanged settled decision must not re-emit.
      await ensureSessionWorktree(host.context, "session-1", noCliDeps);
    });
    const placement = warnings.filter((line) => line.includes("phasegent: placement"));
    expect(placement.filter((line) => line.includes("request "))).toHaveLength(1);
    expect(placement.filter((line) => line.includes("accepted "))).toHaveLength(1);
    // `accepted` is never labelled a confirmation.
    expect(placement.some((line) => line.includes("confirmed"))).toBe(false);
    // Only the bounded allowlist of fields appears.
    const allowed = new Set(["session", "parent", "from", "to", "reason", "outcome"]);
    for (const line of placement) {
      const fields = line.replace(/^phasegent: placement \S+/, "").trim();
      for (const token of fields.split(/\s+/).filter(Boolean)) {
        const key = token.slice(0, token.indexOf("="));
        expect(allowed.has(key)).toBe(true);
      }
    }
  });

  test("a suppressed already-there decision emits one bounded suppression line", async () => {
    const warnings = await withWarnings(async () => {
      rememberWorktree("session-1", WORKTREE);
      const host = recordingHost({
        "session-1": { parentID: null, location: { directory: WORKTREE } },
      });
      await ensureSessionWorktree(host.context, "session-1", noCliDeps);
    });
    const suppressed = warnings.filter(
      (line) =>
        line.includes("phasegent: placement suppressed") &&
        line.includes("reason=already-in-target"),
    );
    expect(suppressed).toHaveLength(1);
    expect(suppressed[0]).toContain("session=session-1");
  });
});

describe("agentRole (issue #541)", () => {
  test("maps known agent names to phasegent roles", () => {
    expect(agentRole({ agent: "orchestrator" })).toBe("orchestrator");
    expect(agentRole({ agent: "executor" })).toBe("executor");
    expect(agentRole({ agent: "reviewer" })).toBe("reviewer");
    // issue #754 P3: explore maps to its own least-privilege role, not the
    // reviewer surface, so it can publish an authorized recon record without
    // gaining reviewer writes.
    expect(agentRole({ agent: "explore" })).toBe("explore");
  });

  test("is case-insensitive with exact canonical ids only (issue #859 P1)", () => {
    expect(agentRole({ agent: "Executor" })).toBe("executor");
    expect(agentRole({ agent: "ORCHESTRATOR" })).toBe("orchestrator");
    // Substring and compound lookalikes never elevate: they are unknown and
    // fail closed as managed-without-role instead of a privileged role.
    expect(agentRole({ agent: "task-executor" })).toBeNull();
    expect(agentRole({ agent: "my-orchestrator" })).toBeNull();
    expect(agentRole({ agent: "reviewer-2" })).toBeNull();
    expect(agentRole({ agent: "explore-plus" })).toBeNull();
  });

  test("never guesses for unknown or missing agents", () => {
    expect(agentRole({ agent: "general" })).toBeNull();
    // The retired verification role is not a session role any more.
    expect(agentRole({ agent: "tester" })).toBeNull();
    expect(agentRole({ agent: "" })).toBeNull();
    expect(agentRole({})).toBeNull();
    expect(agentRole(undefined)).toBeNull();
  });
});

describe("isSubagentSession (issue #541)", () => {
  test("is true only for a resolved non-orchestrator role", () => {
    expect(isSubagentSession({ agent: "orchestrator" })).toBe(false);
    expect(isSubagentSession({ agent: "executor" })).toBe(true);
    expect(isSubagentSession({ agent: "explore" })).toBe(true);
    expect(isSubagentSession({ agent: "general" })).toBe(false);
    expect(isSubagentSession(undefined)).toBe(false);
  });
});

describe("sessionPlaced (issue #541)", () => {
  test("reports false until a move is confirmed", () => {
    expect(sessionPlaced(undefined)).toBe(false);
    expect(sessionPlaced("session-1")).toBe(false);
  });
});

describe("rewritePhasegentCommand (issue #541)", () => {
  test("keeps --session before the pipe of an issue create", () => {
    const out = rewritePhasegentCommand(
      "phasegent issue create --title t --body b | tee /tmp/x",
      "session-1",
      { agent: "orchestrator" },
    );
    expect(out).toBe(
      "PHASEGENT_ROLE=orchestrator phasegent issue create --title t --body b --session session-1 | tee /tmp/x",
    );
  });

  test("keeps an existing role assignment and leaves issue bind untouched", () => {
    expect(
      rewritePhasegentCommand(
        "PHASEGENT_ROLE=executor phasegent --provider local issue bind 18",
        "abc",
        { agent: "orchestrator" },
      ),
    ).toBe("PHASEGENT_ROLE=executor phasegent --provider local issue bind 18");
  });

  test("skips --session when it is already present", () => {
    expect(
      rewritePhasegentCommand(
        "phasegent issue create --title t --session s1",
        "s2",
        undefined,
      ),
    ).toBe("phasegent issue create --title t --session s1");
    expect(
      rewritePhasegentCommand("phasegent issue bind 18 --session=s1", "s2", undefined),
    ).toBe("phasegent issue bind 18 --session=s1");
  });

  test("injects the role before the phasegent token", () => {
    expect(
      rewritePhasegentCommand("phasegent issue status", "s1", { agent: "executor" }),
    ).toBe("PHASEGENT_ROLE=executor phasegent issue status");
    expect(
      rewritePhasegentCommand("phasegent worktree status --issue 1", undefined, {
        agent: "reviewer",
      }),
    ).toBe("PHASEGENT_ROLE=reviewer phasegent worktree status --issue 1");
  });

  test("leaves an existing role assignment untouched", () => {
    expect(
      rewritePhasegentCommand("PHASEGENT_ROLE=reviewer phasegent issue get 1", "s1", {
        agent: "orchestrator",
      }),
    ).toBe("PHASEGENT_ROLE=reviewer phasegent issue get 1");
    expect(
      rewritePhasegentCommand("PHASEGENT_ROLE=admin phasegent issue get 1", "s1", {
        agent: "orchestrator",
      }),
    ).toBe("PHASEGENT_ROLE=admin phasegent issue get 1");
    expect(
      rewritePhasegentCommand("PHASEGENT_ROLE=executor phasegent issue get 1", "s1", {
        agent: "executor",
      }),
    ).toBe("PHASEGENT_ROLE=executor phasegent issue get 1");
  });

  test("scopes the role to the invocation on a Windows host", () => {
    expect(
      rewritePhasegentCommand(
        "phasegent issue status",
        "s1",
        { agent: "executor" },
        { windows: true },
      ),
    ).toBe(windowsScope("phasegent issue status"));
    expect(
      rewritePhasegentCommand(
        "phasegent issue status",
        "s1",
        { agent: "executor" },
        { windows: false },
      ),
    ).toBe("PHASEGENT_ROLE=executor phasegent issue status");
    expect(
      rewritePhasegentCommand(
        "PHASEGENT_ROLE=executor phasegent issue status",
        "s1",
        { agent: "executor" },
        { windows: true },
      ),
    ).toBe("PHASEGENT_ROLE=executor phasegent issue status");
  });

  test("does not leak the PowerShell role into a following statement", () => {
    // Issue #588 P2 review: a bare `$env:PHASEGENT_ROLE=...;` assignment stays
    // set for the rest of the shell, so `Write-Output` and any child process
    // launched afterwards inherited the session role. The scope restores the
    // previous value before the following statement runs.
    const out = rewritePhasegentCommand(
      "phasegent issue get 1; Write-Output $env:PHASEGENT_ROLE",
      "s1",
      { agent: "executor" },
      { windows: true },
    );
    expect(out).toBe(`${windowsScope("phasegent issue get 1")}; Write-Output $env:PHASEGENT_ROLE`);
    const restore = out.indexOf("$env:PHASEGENT_ROLE=$__phasegent_role");
    expect(restore).toBeGreaterThan(-1);
    expect(restore).toBeLessThan(out.indexOf("Write-Output"));
    expect(out.slice(restore)).not.toContain("'executor'");
  });

  test("does not leak the PowerShell role into a following child process", () => {
    const out = rewritePhasegentCommand(
      "phasegent issue get 1; cmd /c echo %PHASEGENT_ROLE%",
      "s1",
      { agent: "executor" },
      { windows: true },
    );
    const injected = out.indexOf("$env:PHASEGENT_ROLE='executor'");
    const restore = out.indexOf("$env:PHASEGENT_ROLE=$__phasegent_role");
    const child = out.indexOf("cmd /c");
    expect(injected).toBeGreaterThan(-1);
    expect(restore).toBeGreaterThan(injected);
    expect(child).toBeGreaterThan(restore);
    // The child command runs after the restore, so the role literal is gone.
    expect(out.slice(child)).not.toContain("'executor'");
  });

  test("scopes each Windows invocation independently", () => {
    expect(
      rewritePhasegentCommand(
        "phasegent issue status && phasegent issue get 1",
        "s1",
        { agent: "executor" },
        { windows: true },
      ),
    ).toBe(`${windowsScope("phasegent issue status")} && ${windowsScope("phasegent issue get 1")}`);
    expect(
      rewritePhasegentCommand(
        "phasegent issue status | tee /tmp/x",
        "s1",
        { agent: "executor" },
        { windows: true },
      ),
    ).toBe(`${windowsScope("phasegent issue status")} | tee /tmp/x`);
  });

  test("preserves the CLI failure status across the PowerShell restore (&&)", () => {
    // Issue #588 P2 round 3: the successful restore assignment must not make a
    // failing CLI look successful, so the captured nonzero `$LASTEXITCODE` is
    // re-asserted after the restore. The scope is a `$( … )` subexpression (not
    // `& { … }`, which resets `$?` on its own), so the failure survives into the
    // `&&` evaluation.
    const out = rewritePhasegentCommand(
      "phasegent issue get 1 && Write-Output done",
      "s1",
      { agent: "executor" },
      { windows: true },
    );
    expect(out).toBe(`${windowsScope("phasegent issue get 1")} && Write-Output done`);
    expect(out.startsWith("$( ")).toBe(true);
    expect(out).not.toContain("& { ");
    const status = out.indexOf("$__phasegent_status=$LASTEXITCODE");
    const restore = out.indexOf("$env:PHASEGENT_ROLE=$__phasegent_role");
    const reassert = out.indexOf(
      "Write-Error -Message 'phasegent failed' -ErrorAction SilentlyContinue",
    );
    const chain = out.indexOf(") && Write-Output done");
    expect(status).toBeGreaterThan(-1);
    expect(restore).toBeGreaterThan(status);
    expect(reassert).toBeGreaterThan(restore);
    expect(chain).toBeGreaterThan(reassert);
  });

  test("preserves the CLI failure status before a PowerShell || fallback", () => {
    const out = rewritePhasegentCommand(
      "phasegent issue get 1 || Write-Output fallback",
      "s1",
      { agent: "executor" },
      { windows: true },
    );
    expect(out).toBe(`${windowsScope("phasegent issue get 1")} || Write-Output fallback`);
    expect(out.indexOf(") || Write-Output fallback")).toBeGreaterThan(
      out.indexOf("Write-Error"),
    );
  });

  test("restores the PowerShell role even when the CLI fails", () => {
    const out = rewritePhasegentCommand(
      "phasegent issue get 1",
      "s1",
      { agent: "executor" },
      { windows: true },
    );
    // `finally` runs on a terminating CLI failure instead of leaving the role
    // set for the rest of the shell, and captures the status before restoring.
    expect(out).toContain("try { $env:PHASEGENT_ROLE='executor'; ");
    expect(out).toContain(
      "finally { $__phasegent_status=$LASTEXITCODE; $env:PHASEGENT_ROLE=$__phasegent_role;",
    );
  });

  test("leaves a removed --role flag for the CLI to reject", () => {
    // The flag is intentionally incompatible (issue #588): the adapter adds the
    // role assignment and never rewrites the flag itself.
    expect(
      rewritePhasegentCommand("phasegent --role orchestrator issue status", "s1", {
        agent: "executor",
      }),
    ).toBe("PHASEGENT_ROLE=executor phasegent --role orchestrator issue status");
  });

  test("does not inject a role for an unknown agent", () => {
    expect(
      rewritePhasegentCommand("phasegent issue create --title t", "s1", {
        agent: "general",
      }),
    ).toBe("phasegent issue create --title t --session s1");
    expect(
      rewritePhasegentCommand("phasegent issue status", "s1", { agent: "general" }),
    ).toBe("phasegent issue status");
  });

  test("appends --session only to an issue create segment", () => {
    expect(
      rewritePhasegentCommand("phasegent issue status", "s1", undefined),
    ).toBe("phasegent issue status");
    expect(rewritePhasegentCommand("ls -la", "s1", undefined)).toBe("ls -la");
    expect(
      rewritePhasegentCommand("phasegent worktree acquire --issue 18", "s1", undefined),
    ).toBe("phasegent worktree acquire --issue 18");
    expect(
      rewritePhasegentCommand("phasegent issue bind 18", "s1", undefined),
    ).toBe("phasegent issue bind 18");
  });

  test("passes through without a session id", () => {
    const command = "phasegent issue create --title t";
    expect(rewritePhasegentCommand(command, undefined, undefined)).toBe(command);
    expect(rewritePhasegentCommand(command, "", undefined)).toBe(command);
    expect(rewritePhasegentCommand(undefined, "s1", undefined)).toBeUndefined();
  });

  test("rewrites every segment of a compound command", () => {
    expect(
      rewritePhasegentCommand(
        "phasegent issue get 1 && phasegent issue bind 541",
        "s9",
        { agent: "orchestrator" },
      ),
    ).toBe(
      "PHASEGENT_ROLE=orchestrator phasegent issue get 1 && PHASEGENT_ROLE=orchestrator phasegent issue bind 541",
    );
  });

  test("rewrites an invocation behind env assignments or a path prefix", () => {
    expect(
      rewritePhasegentCommand(
        "PHASEGENT_WORKTREE_NO_DISCOVER=1 phasegent issue status",
        "s1",
        { agent: "executor" },
      ),
    ).toBe("PHASEGENT_WORKTREE_NO_DISCOVER=1 PHASEGENT_ROLE=executor phasegent issue status");
    expect(
      rewritePhasegentCommand("./phasegent issue status", "s1", { agent: "executor" }),
    ).toBe("PHASEGENT_ROLE=executor ./phasegent issue status");
  });

  test("does not rewrite phasegent look-alikes", () => {
    expect(
      rewritePhasegentCommand("grep -rn phasegent src", "s1", { agent: "executor" }),
    ).toBe("grep -rn phasegent src");
    expect(
      rewritePhasegentCommand("echo phasegent issue create", "s1", { agent: "executor" }),
    ).toBe("echo phasegent issue create");
    expect(
      rewritePhasegentCommand("git -C repo phasegent issue status", "s1", {
        agent: "executor",
      }),
    ).toBe("git -C repo phasegent issue status");
  });

  test("refuses issue create|bind for a sub-agent session", () => {
    for (const command of [
      "phasegent issue create --title t --body b",
      "phasegent issue bind 541",
      // A claimed orchestrator role must not buy a sub-agent an issue write.
      "PHASEGENT_ROLE=orchestrator phasegent issue create --title t",
    ]) {
      const out = rewritePhasegentCommand(command, "s1", { agent: "executor" });
      expect(out).toContain("cannot run 'issue create|bind'");
      expect(out.endsWith("; false")).toBe(true);
      expect(out).not.toContain("phasegent issue create");
      expect(out).not.toContain("--title");
    }
  });

  test("downgrades a claimed orchestrator/admin role to the session role", () => {
    // The close segment also carries its closer flag (issue #575 P2); the CLI
    // role gate still refuses a close that is not run as an orchestrator.
    expect(
      rewritePhasegentCommand("PHASEGENT_ROLE=orchestrator phasegent issue close 1", "s1", {
        agent: "executor",
      }),
    ).toBe("PHASEGENT_ROLE=executor phasegent issue close 1 --worktree-session s1");
    expect(
      rewritePhasegentCommand("PHASEGENT_ROLE=ADMIN phasegent issue status", "s1", {
        agent: "reviewer",
      }),
    ).toBe("PHASEGENT_ROLE=reviewer phasegent issue status");
  });

  test("outranks a quoted orchestrator/admin env claim for a sub-agent", () => {
    // A quoted value is never rewritten in place, so the injected session-role
    // assignment comes last and wins.
    expect(
      rewritePhasegentCommand(
        "PHASEGENT_ROLE='orchestrator' phasegent issue status",
        "s1",
        { agent: "executor" },
      ),
    ).toBe("PHASEGENT_ROLE='orchestrator' PHASEGENT_ROLE=executor phasegent issue status");
  });

  test("outranks a differing role claim for a sub-agent", () => {
    expect(
      rewritePhasegentCommand(
        "PHASEGENT_ROLE=reviewer phasegent issue status",
        "s1",
        { agent: "executor" },
      ),
    ).toBe("PHASEGENT_ROLE=reviewer PHASEGENT_ROLE=executor phasegent issue status");
  });

  test("keeps an orchestrator session's own role claim", () => {
    // `issue close` additionally gets its closer flag (issue #575 P2).
    expect(
      rewritePhasegentCommand("PHASEGENT_ROLE=orchestrator phasegent issue close 1", "s1", {
        agent: "orchestrator",
      }),
    ).toBe("PHASEGENT_ROLE=orchestrator phasegent issue close 1 --worktree-session s1");
  });

  test("injects the least-privilege role for the structured record group (issue #754 P3)", () => {
    // The record group carries no session flag, so the only rewrite is the
    // role scope. An explore sub-agent runs as `explore` (not `reviewer`), and
    // a differing claim is outranked by the injected session role.
    expect(
      rewritePhasegentCommand(
        "phasegent record create 754 --kind recon --key scan-1 --recon repo --authorized --body t",
        "s1",
        { agent: "explore" },
      ),
    ).toBe(
      "PHASEGENT_ROLE=explore phasegent record create 754 --kind recon --key scan-1 --recon repo --authorized --body t",
    );
    expect(
      rewritePhasegentCommand(
        "PHASEGENT_ROLE=reviewer phasegent record create 754 --kind reviewer --key r-1 --authorized --body t",
        "s1",
        { agent: "explore" },
      ),
    ).toBe(
      "PHASEGENT_ROLE=reviewer PHASEGENT_ROLE=explore phasegent record create 754 --kind reviewer --key r-1 --authorized --body t",
    );
    expect(
      rewritePhasegentCommand(
        "phasegent record create 754 --kind executor --key e-1 --phase P1 --attempt 1 --body t",
        "s1",
        { agent: "executor" },
      ),
    ).toBe(
      "PHASEGENT_ROLE=executor phasegent record create 754 --kind executor --key e-1 --phase P1 --attempt 1 --body t",
    );
    // A record read is still a phasegent invocation, so it gets the role too.
    expect(
      rewritePhasegentCommand("phasegent record get 754 42", "s1", { agent: "explore" }),
    ).toBe("PHASEGENT_ROLE=explore phasegent record get 754 42");
  });
});

// Real PowerShell 7 regressions for the Windows role scope (issue #588 P2):
// the emitted command runs unchanged against a fake `phasegent` on PATH, so
// `&&`, `||`, and the restored environment state are exercised for real rather
// than only matched as text. Skipped where PowerShell 7 is unavailable (the CI
// Linux image) and run locally through `PHASEGENT_PWSH`.
if (PWSH_PATH && process.platform !== "win32") {
  describe("PowerShell role scope runtime (issue #588 P2)", () => {
    let dir;

    beforeEach(async () => {
      dir = await mkdtemp(join(tmpdir(), "phasegent-pwsh-"));
      const fake = join(dir, "phasegent");
      await writeFile(
        fake,
        '#!/usr/bin/env bash\nprintf "role=%s\\n" "${PHASEGENT_ROLE:-<absent>}"\nexit "${FAKE_CODE:-0}"\n',
      );
      await chmod(fake, 0o755);
    });

    afterEach(async () => {
      await rm(dir, { recursive: true, force: true });
    });

    function run(command, options) {
      const { code = 0, role } = options || {};
      const prelude = `$env:PHASEGENT_ROLE=${role ? `'${role}'` : "$null"}\n`;
      const rewritten = rewritePhasegentCommand(
        command,
        "s1",
        { agent: "executor" },
        { windows: true },
      );
      const script =
        `${prelude}${rewritten}\n` +
        `Write-Output "final=<$env:PHASEGENT_ROLE> code=$LASTEXITCODE"\n`;
      const result = spawnSync(PWSH_PATH, ["-NoProfile", "-Command", script], {
        env: { ...process.env, PATH: `${dir}:${process.env.PATH}`, FAKE_CODE: String(code) },
        encoding: "utf8",
      });
      return { stdout: result.stdout || "", stderr: result.stderr || "", status: result.status };
    }

    test("a failing invocation skips && and runs ||", () => {
      const and = run("phasegent issue get 1 && Write-Output done", { code: 3 });
      expect(and.stdout).toContain("role=executor");
      expect(and.stdout).not.toContain("done");
      expect(and.stdout).toContain("final=<> code=3");

      const or = run("phasegent issue get 1 || Write-Output fallback", { code: 3 });
      expect(or.stdout).toContain("fallback");
      expect(or.stdout).toContain("final=<> code=3");
    });

    test("a successful invocation runs && and restores a pre-existing role", () => {
      const ok = run("phasegent issue get 1 && Write-Output done", { code: 0, role: "admin" });
      expect(ok.stdout).toContain("role=executor");
      expect(ok.stdout).toContain("done");
      expect(ok.stdout).toContain("final=<admin> code=0");
    });

    test("a failing invocation restores a pre-existing role", () => {
      const out = run("phasegent issue get 1 || Write-Output fallback", { code: 4, role: "admin" });
      expect(out.stdout).toContain("fallback");
      expect(out.stdout).toContain("final=<admin> code=4");
    });
  });
}

// Issue #917 P1: bounded POSIX transparent wrappers (`timeout`, `env`,
// `command`, nested) between the leading assignments and the command name.
// The role is injected through `env` so the wrapper still execs a real command
// rather than an assignment; unsupported/ambiguous shapes stay byte-for-byte.
describe("POSIX transparent wrappers (issue #917 P1)", () => {
  test("injects the role through env behind a timeout wrapper", () => {
    expect(
      rewritePhasegentCommand("timeout 40s phasegent issue get 1", "s1", { agent: "executor" }),
    ).toBe("timeout 40s env PHASEGENT_ROLE=executor phasegent issue get 1");
  });

  test("keeps a user assignment before the wrapper and the session flag after", () => {
    expect(
      rewritePhasegentCommand(
        "TMPDIR=/tmp timeout 40s phasegent issue create --title t",
        "s1",
        { agent: "orchestrator" },
      ),
    ).toBe(
      "TMPDIR=/tmp timeout 40s env PHASEGENT_ROLE=orchestrator phasegent issue create --title t --session s1",
    );
  });

  test("handles separated, equals, flag and duration timeout options", () => {
    expect(
      rewritePhasegentCommand(
        "timeout --signal=KILL -k 1s --preserve-status 40s phasegent issue status",
        "s1",
        { agent: "executor" },
      ),
    ).toBe(
      "timeout --signal=KILL -k 1s --preserve-status 40s env PHASEGENT_ROLE=executor phasegent issue status",
    );
    expect(
      rewritePhasegentCommand("timeout -s TERM 40s -- phasegent issue status", "s1", {
        agent: "executor",
      }),
    ).toBe("timeout -s TERM 40s -- env PHASEGENT_ROLE=executor phasegent issue status");
  });

  test("handles env assignments and command wrappers nested with timeout", () => {
    expect(
      rewritePhasegentCommand("env FOO=1 phasegent issue status", "s1", { agent: "executor" }),
    ).toBe("env FOO=1 env PHASEGENT_ROLE=executor phasegent issue status");
    expect(
      rewritePhasegentCommand("command phasegent issue status", "s1", { agent: "executor" }),
    ).toBe("command env PHASEGENT_ROLE=executor phasegent issue status");
    expect(
      rewritePhasegentCommand("timeout 5s env FOO=1 command phasegent issue status", "s1", {
        agent: "reviewer",
      }),
    ).toBe("timeout 5s env FOO=1 command env PHASEGENT_ROLE=reviewer phasegent issue status");
  });

  test("is idempotent on a rewritten env prefix", () => {
    const once = rewritePhasegentCommand("timeout 40s phasegent issue get 1", "s1", {
      agent: "executor",
    });
    expect(rewritePhasegentCommand(once, "s1", { agent: "executor" })).toBe(once);
    const userAssignment = rewritePhasegentCommand(
      "PHASEGENT_ROLE=executor timeout 40s phasegent issue get 1",
      "s1",
      { agent: "executor" },
    );
    expect(userAssignment).toBe("PHASEGENT_ROLE=executor timeout 40s phasegent issue get 1");
  });

  test("leaves unsupported or ambiguous wrappers byte-for-byte", () => {
    for (const command of [
      "sudo phasegent issue status",
      "sh -c phasegent issue status",
      "xargs phasegent",
      "timeout --bogus 40s phasegent issue status",
      "timeout 40s",
      "command -v phasegent",
    ]) {
      expect(rewritePhasegentCommand(command, "s1", { agent: "executor" })).toBe(command);
    }
  });

  test("downgrades a claimed role and refuses issue create behind a wrapper", () => {
    expect(
      rewritePhasegentCommand(
        "PHASEGENT_ROLE=orchestrator timeout 5s phasegent issue status",
        "s1",
        { agent: "executor" },
      ),
    ).toBe("PHASEGENT_ROLE=executor timeout 5s phasegent issue status");
    const refused = rewritePhasegentCommand("timeout 5s phasegent issue create --title t", "s1", {
      agent: "executor",
    });
    expect(refused).toContain("cannot run 'issue create|bind'");
    expect(refused).not.toContain("--title");
  });

  test("leaves a wrapped invocation untouched on a Windows host", () => {
    expect(
      rewritePhasegentCommand(
        "timeout 40s phasegent issue status",
        "s1",
        { agent: "executor" },
        { windows: true },
      ),
    ).toBe("timeout 40s phasegent issue status");
  });

  test("injects the session role for every canonical role behind a wrapper", () => {
    for (const agent of ["orchestrator", "executor", "reviewer", "explore"]) {
      expect(rewritePhasegentCommand("timeout 5s phasegent issue get 1", "s1", { agent })).toBe(
        `timeout 5s env PHASEGENT_ROLE=${agent} phasegent issue get 1`,
      );
    }
  });

  test("leaves quoted data and heredoc bodies untouched behind a wrapper", () => {
    expect(
      rewritePhasegentCommand('timeout 5s phasegent issue get 1 --body "a && b"', "s1", {
        agent: "executor",
      }),
    ).toBe('timeout 5s env PHASEGENT_ROLE=executor phasegent issue get 1 --body "a && b"');
    const heredoc =
      "timeout 5s phasegent issue get 1 <<'EOF'\n" +
      "timeout 5s PHASEGENT_ROLE=admin phasegent issue status\n" +
      "EOF";
    expect(rewritePhasegentCommand(heredoc, "s1", { agent: "executor" })).toBe(
      "timeout 5s env PHASEGENT_ROLE=executor phasegent issue get 1 <<'EOF'\n" +
        "timeout 5s PHASEGENT_ROLE=admin phasegent issue status\n" +
        "EOF",
    );
  });
});

// The rewritten POSIX strings run unchanged against a fake `phasegent` on
// PATH, so the `env` wrapper, wrapper syntax and exit-code propagation are
// exercised for real (issue #917 P1).
if (process.platform !== "win32") {
  describe("POSIX wrapper runtime (issue #917 P1)", () => {
    let dir;
    const hasTimeout =
      spawnSync("/bin/sh", ["-c", "command -v timeout"], { encoding: "utf8" }).status === 0;
    const runtimeTest = hasTimeout ? test : test.skip;

    beforeEach(async () => {
      dir = await mkdtemp(join(tmpdir(), "phasegent-posix-"));
      const fake = join(dir, "phasegent");
      await writeFile(
        fake,
        '#!/bin/sh\nprintf "role=%s\\n" "${PHASEGENT_ROLE-<absent>}"\nexit "${FAKE_CODE:-0}"\n',
      );
      await chmod(fake, 0o755);
    });

    afterEach(async () => {
      await rm(dir, { recursive: true, force: true });
    });

    function run(command, options) {
      const { code = 0, agent = "executor" } = options || {};
      const rewritten = rewritePhasegentCommand(command, "s1", { agent });
      const env = {
        ...process.env,
        PATH: `${dir}:${process.env.PATH}`,
        FAKE_CODE: String(code),
      };
      // The ambient role must not leak in: the assertion below proves the
      // per-invocation scope is what sets (and clears) PHASEGENT_ROLE.
      delete env.PHASEGENT_ROLE;
      return spawnSync("/bin/sh", ["-c", rewritten], { env, encoding: "utf8" });
    }

    runtimeTest("injects the session role through each wrapper shape", () => {
      for (const command of [
        "timeout 40s phasegent issue get 1",
        "TMPDIR=/tmp timeout 40s phasegent issue get 1",
        "env FOO=1 phasegent issue get 1",
        "command phasegent issue get 1",
        "timeout 5s env FOO=1 phasegent issue get 1",
      ]) {
        const result = run(command);
        expect(result.stdout).toContain("role=executor");
        expect(result.status).toBe(0);
      }
    });

    runtimeTest("propagates a wrapped CLI failure through && and ||", () => {
      const and = run("timeout 40s phasegent issue get 1 && echo done", { code: 3 });
      expect(and.stdout).toContain("role=executor");
      expect(and.stdout).not.toContain("done");
      const or = run("timeout 40s phasegent issue get 1 || echo fallback", { code: 3 });
      expect(or.stdout).toContain("fallback");
    });

    runtimeTest("scopes the role to the wrapped child only", () => {
      const out = run("timeout 40s phasegent issue get 1 && echo after=$PHASEGENT_ROLE");
      expect(out.stdout).toContain("after=");
      expect(out.stdout.trimEnd().endsWith("after=")).toBe(true);
    });
  });
}

describe("issue close session injection (issue #575 P2)", () => {
  test("appends --worktree-session to an issue close segment", () => {
    expect(
      rewritePhasegentCommand("phasegent issue close 575", "session-1", {
        agent: "orchestrator",
      }),
    ).toBe(
      "PHASEGENT_ROLE=orchestrator phasegent issue close 575 --worktree-session session-1",
    );
  });

  test("uses --worktree-session, never --session, on a close segment", () => {
    // `issue close` rejects `--session` with `unknown option` (exit 2), so the
    // injected flag must be the one the CLI parses.
    const out = rewritePhasegentCommand("phasegent issue close 575", "session-1", undefined);
    expect(out).toBe("phasegent issue close 575 --worktree-session session-1");
    expect(out.includes("--session")).toBe(false);
  });

  test("skips the flag when --worktree-session is already present", () => {
    expect(
      rewritePhasegentCommand("phasegent issue close 575 --worktree-session s1", "s2", undefined),
    ).toBe("phasegent issue close 575 --worktree-session s1");
    expect(
      rewritePhasegentCommand("phasegent issue close 575 --worktree-session=s1", "s2", undefined),
    ).toBe("phasegent issue close 575 --worktree-session=s1");
  });

  test("injects per segment in a compound close command", () => {
    const expected =
      "PHASEGENT_ROLE=orchestrator phasegent issue close 575 --worktree-session s1 && " +
      "PHASEGENT_ROLE=orchestrator phasegent issue sync";
    expect(
      rewritePhasegentCommand("phasegent issue close 575 && phasegent issue sync", "s1", {
        agent: "orchestrator",
      }),
    ).toBe(expected);
  });

  test("keeps a quoted --worktree-session value and adds nothing", () => {
    expect(
      rewritePhasegentCommand('phasegent issue close 575 --worktree-session "s1 s2"', "s9", {
        agent: "orchestrator",
      }),
    ).toBe(
      'PHASEGENT_ROLE=orchestrator phasegent issue close 575 --worktree-session "s1 s2"',
    );
  });

  test("adds the close flag only when a session id is resolved", () => {
    const command = "phasegent issue close 575";
    expect(rewritePhasegentCommand(command, undefined, undefined)).toBe(command);
    expect(rewritePhasegentCommand(command, "", undefined)).toBe(command);
  });

  test("keeps --session on create segments only", () => {
    expect(
      rewritePhasegentCommand("phasegent issue create --title t", "s1", undefined),
    ).toBe("phasegent issue create --title t --session s1");
    expect(
      rewritePhasegentCommand("phasegent issue bind 575", "s1", undefined),
    ).toBe("phasegent issue bind 575");
    expect(rewritePhasegentCommand("phasegent issue close 575", "s1", undefined)).toBe(
      "phasegent issue close 575 --worktree-session s1",
    );
    expect(
      rewritePhasegentCommand("phasegent issue status", "s1", undefined),
    ).toBe("phasegent issue status");
  });
});

describe("quote-aware command rewriting (issue #541 P1)", () => {
  test("keeps --session outside a quoted pipe in --body", () => {
    expect(
      rewritePhasegentCommand('phasegent issue create --title t --body "a | b"', "session-1", {
        agent: "orchestrator",
      }),
    ).toBe(
      'PHASEGENT_ROLE=orchestrator phasegent issue create --title t --body "a | b" --session session-1',
    );
  });

  test("keeps --session outside a multiline --body", () => {
    // A real newline inside the value must not split the segment.
    const command = 'phasegent issue create --title t --body "l1\nl2"';
    expect(rewritePhasegentCommand(command, "session-1", { agent: "orchestrator" })).toBe(
      'PHASEGENT_ROLE=orchestrator phasegent issue create --title t --body "l1\nl2" --session session-1',
    );
  });

  test("keeps --session outside a quoted && in --title", () => {
    expect(
      rewritePhasegentCommand('phasegent issue create --title "a && b"', "session-1", {
        agent: "orchestrator",
      }),
    ).toBe(
      'PHASEGENT_ROLE=orchestrator phasegent issue create --title "a && b" --session session-1',
    );
  });

  test("leaves an unterminated quote segment byte-for-byte", () => {
    for (const command of [
      'phasegent issue create --body "a | b',
      "phasegent issue create --title 'a && b",
      'phasegent issue status --body "a  ',
    ]) {
      expect(
        rewritePhasegentCommand(command, "session-1", { agent: "orchestrator" }),
      ).toBe(command);
    }
  });

  test("refuses issue create for a sub-agent even with an unterminated quote", () => {
    const out = rewritePhasegentCommand('phasegent issue create --body "a', "s1", {
      agent: "executor",
    });
    expect(out).toContain("cannot run 'issue create|bind'");
    expect(out.endsWith("; false")).toBe(true);
  });

  test("does not mistake a quoted --session value for the real flag", () => {
    expect(
      rewritePhasegentCommand(
        'phasegent issue create --title t --body "--session s9"',
        "session-1",
        { agent: "orchestrator" },
      ),
    ).toBe(
      'PHASEGENT_ROLE=orchestrator phasegent issue create --title t --body "--session s9" --session session-1',
    );
  });

  test("does not mistake a quoted issue bind for an issue write", () => {
    expect(
      rewritePhasegentCommand('phasegent issue get 1 --body "issue bind"', "session-1", {
        agent: "executor",
      }),
    ).toBe('PHASEGENT_ROLE=executor phasegent issue get 1 --body "issue bind"');
  });

  test("does not rewrite a quoted --role value for a sub-agent", () => {
    expect(
      rewritePhasegentCommand('phasegent issue get 1 --body "--role orchestrator"', "s1", {
        agent: "executor",
      }),
    ).toBe('PHASEGENT_ROLE=executor phasegent issue get 1 --body "--role orchestrator"');
  });

  test("does not rewrite a quoted PHASEGENT_ROLE value for a sub-agent", () => {
    expect(
      rewritePhasegentCommand(
        'phasegent issue get 1 --body "PHASEGENT_ROLE=orchestrator"',
        "s1",
        { agent: "executor" },
      ),
    ).toBe(
      'PHASEGENT_ROLE=executor phasegent issue get 1 --body "PHASEGENT_ROLE=orchestrator"',
    );
  });

  test("does not read a quoted role literal before the invocation as an assignment", () => {
    expect(
      rewritePhasegentCommand(
        'FOO="PHASEGENT_ROLE=admin" phasegent issue get 1',
        "s1",
        { agent: "executor" },
      ),
    ).toBe('FOO="PHASEGENT_ROLE=admin" PHASEGENT_ROLE=executor phasegent issue get 1');
  });

  test("handles single-quoted values and escaped quotes", () => {
    expect(
      rewritePhasegentCommand(
        "phasegent issue create --title t --body 'a | b'",
        "s1",
        { agent: "orchestrator" },
      ),
    ).toBe(
      "PHASEGENT_ROLE=orchestrator phasegent issue create --title t --body 'a | b' --session s1",
    );
    expect(
      rewritePhasegentCommand(
        'phasegent issue create --title t --body "a \\" | b"',
        "s1",
        { agent: "orchestrator" },
      ),
    ).toBe(
      'PHASEGENT_ROLE=orchestrator phasegent issue create --title t --body "a \\" | b" --session s1',
    );
  });

  test("still splits a real pipe and leaves tee untouched", () => {
    expect(
      rewritePhasegentCommand(
        "phasegent issue create --title t --body b | tee /tmp/x",
        "session-1",
        { agent: "orchestrator" },
      ),
    ).toBe(
      "PHASEGENT_ROLE=orchestrator phasegent issue create --title t --body b --session session-1 | tee /tmp/x",
    );
    expect(
      rewritePhasegentCommand(
        'phasegent issue bind 541 --note "a && b" | tee /tmp/y',
        "s9",
        { agent: "orchestrator" },
      ),
    ).toBe(
      'PHASEGENT_ROLE=orchestrator phasegent issue bind 541 --note "a && b" | tee /tmp/y',
    );
  });

  test("still splits segments outside quotes around quoted content", () => {
    expect(
      rewritePhasegentCommand(
        'phasegent issue status; echo "a | b"; phasegent worktree status',
        "s1",
        { agent: "executor" },
      ),
    ).toBe(
      'PHASEGENT_ROLE=executor phasegent issue status; echo "a | b"; PHASEGENT_ROLE=executor phasegent worktree status',
    );
  });

  test("still recognises a quoted env value before the invocation", () => {
    expect(
      rewritePhasegentCommand(
        'PHASEGENT_WORKTREE_NO_DISCOVER="1" phasegent issue status',
        "s1",
        { agent: "executor" },
      ),
    ).toBe(
      'PHASEGENT_WORKTREE_NO_DISCOVER="1" PHASEGENT_ROLE=executor phasegent issue status',
    );
  });
});

describe("heredoc-opaque rewriting (issue #859 P2)", () => {
  const EXPLORE = { agent: "explore" };

  test("incident pattern: quoted heredoc with apostrophe leaves body and rewrites trailing recon", () => {
    const command =
      "cd /tmp && cat > /tmp/body.txt <<'BODY'\nit's the body with apostrophe\nBODY\nphasegent record create 853 --kind recon --recon mixed --key k --authorized --body-file /tmp/body.txt";
    const out = rewritePhasegentCommand(command, "ses-1", EXPLORE);
    expect(out).toBe(
      "cd /tmp && cat > /tmp/body.txt <<'BODY'\nit's the body with apostrophe\nBODY\nPHASEGENT_ROLE=explore phasegent record create 853 --kind recon --recon mixed --key k --authorized --body-file /tmp/body.txt",
    );
    expect(out).toContain("it's the body with apostrophe");
    // No role injected inside the body.
    expect(out.split("it's the body")[0]).not.toContain("PHASEGENT_ROLE=explore phasegent record");
  });

  test("quoted, unquoted, and mixed delimiters keep bodies opaque", () => {
    const cases = [
      ["<<'EOF'", "EOF", "it's quoted\nphasegent issue get 9"],
      ['<<"EOF"', "EOF", 'body with "quotes"\nphasegent issue get 9'],
      ["<<EOF", "EOF", "plain body\nphasegent issue get 9"],
      ["<<'A'", "A", "mixed bodies\nphasegent issue get 9"],
    ];
    for (const [decl, terminator, body] of cases) {
      const out = rewritePhasegentCommand(`cat ${decl}\n${body}\n${terminator}\nphasegent issue get 1`, "s1", EXPLORE);
      expect(out).toContain(body.split("\n")[0]);
      expect(out.endsWith("PHASEGENT_ROLE=explore phasegent issue get 1")).toBe(true);
      expect(out).toContain(`${decl}\n${body}\n${terminator}`);
    }
  });

  test("<<- tab stripping honors leading tabs but not spaces", () => {
    const tabbed = "cat <<-EOF\n\tbody\n\tEOF\nphasegent issue get 1";
    expect(rewritePhasegentCommand(tabbed, "s1", EXPLORE)).toBe(
      "cat <<-EOF\n\tbody\n\tEOF\nPHASEGENT_ROLE=explore phasegent issue get 1",
    );
    // Leading spaces do not terminate <<-: conservatively preserved.
    const spaced = "cat <<-EOF\n   body\n   EOF\nphasegent issue get 1";
    expect(rewritePhasegentCommand(spaced, "s1", EXPLORE)).toBe(spaced);
  });

  test("multiple queued bodies in order stay opaque and trailing rewrites", () => {
    const command = "cat <<A\nbodyA\nA\ncat <<B\nbodyB\nB\nphasegent issue get 1";
    expect(rewritePhasegentCommand(command, "s1", EXPLORE)).toBe(
      "cat <<A\nbodyA\nA\ncat <<B\nbodyB\nB\nPHASEGENT_ROLE=explore phasegent issue get 1",
    );
    const queued = "cat <<A && cat <<B\nbodyA\nA\nbodyB\nB\nphasegent issue get 1";
    const out = rewritePhasegentCommand(queued, "s1", EXPLORE);
    expect(out.endsWith("PHASEGENT_ROLE=explore phasegent issue get 1")).toBe(true);
    expect(out).toContain("bodyA\nA\nbodyB\nB");
  });

  test("fake elevated invocations and role claims in bodies stay untouched", () => {
    const command =
      "cat <<'EOF'\nphasegent issue create --title t --body b\nPHASEGENT_ROLE=orchestrator phasegent issue get 9\nADMIN phasegent issue get 1\nEOF\nphasegent issue get 2";
    const out = rewritePhasegentCommand(command, "s1", EXPLORE);
    expect(out).toBe(
      "cat <<'EOF'\nphasegent issue create --title t --body b\nPHASEGENT_ROLE=orchestrator phasegent issue get 9\nADMIN phasegent issue get 1\nEOF\nPHASEGENT_ROLE=explore phasegent issue get 2",
    );
    // The downgrade path also leaves body literals alone for subagents.
    const sub = rewritePhasegentCommand(command, "s1", { agent: "executor" });
    expect(sub).toContain("PHASEGENT_ROLE=orchestrator phasegent issue get 9");
    expect(sub.endsWith("PHASEGENT_ROLE=executor phasegent issue get 2")).toBe(true);
  });

  test("here-strings and quoted operators are not heredocs", () => {
    expect(
      rewritePhasegentCommand('cat <<< "hello phasegent issue get 9"\nphasegent issue get 1', "s1", EXPLORE),
    ).toBe('cat <<< "hello phasegent issue get 9"\nPHASEGENT_ROLE=explore phasegent issue get 1');
    expect(rewritePhasegentCommand('"<<"EOF""\nphasegent issue get 1', "s1", EXPLORE)).toContain(
      "PHASEGENT_ROLE=explore phasegent issue get 1",
    );
    expect(rewritePhasegentCommand("# cat <<EOF\nphasegent issue get 1", "s1", EXPLORE)).toBe(
      "# cat <<EOF\nPHASEGENT_ROLE=explore phasegent issue get 1",
    );
  });

  test("escaped delimiters and quoted declaration text stay code-safe", () => {
    expect(rewritePhasegentCommand("cat <<\\EOF\nbody\nEOF\nphasegent issue get 1", "s1", EXPLORE)).toBe(
      "cat <<\\EOF\nbody\nEOF\nPHASEGENT_ROLE=explore phasegent issue get 1",
    );
    expect(rewritePhasegentCommand('echo "<<EOF"\nphasegent issue get 1', "s1", EXPLORE)).toBe(
      'echo "<<EOF"\nPHASEGENT_ROLE=explore phasegent issue get 1',
    );
  });

  test("unterminated heredoc conservatively preserves the tail", () => {
    const command = "cat <<EOF\nbody without terminator\nphasegent issue get 1";
    expect(rewritePhasegentCommand(command, "s1", EXPLORE)).toBe(command);
    expect(scanHeredocRanges(command).length).toBeGreaterThan(0);
  });

  test("real commands before bodies still scope", () => {
    expect(
      rewritePhasegentCommand("phasegent issue get 0\ncat <<EOF\nbody\nEOF\nphasegent issue get 1", "s1", EXPLORE),
    ).toBe(
      "PHASEGENT_ROLE=explore phasegent issue get 0\ncat <<EOF\nbody\nEOF\nPHASEGENT_ROLE=explore phasegent issue get 1",
    );
  });

  test("scanHeredocRanges, maskQuoted, shellSegments, and transformCodeOnly stay consistent", () => {
    const command = "cat <<'EOF'\nit's data; phasegent issue get 9\nEOF\nphasegent issue get 1";
    const ranges = scanHeredocRanges(command);
    expect(ranges.length).toBeGreaterThan(0);
    const { masked, balanced } = maskQuoted(command);
    expect(balanced).toBe(true);
    expect(masked).not.toContain("phasegent issue get 9");
    expect(masked).toContain("phasegent issue get 1");
    const segments = shellSegments(command);
    expect(segments.length).toBe(2);
    expect(segments[1].text).toContain("phasegent issue get 1");
    const downgraded = transformCodeOnly(command, (span) =>
      span.replace(/(^|[;&|()\s])(PHASEGENT_ROLE=)(orchestrator|admin)\b/gi, "$1$2explore"),
    );
    expect(downgraded).toContain("it's data");
  });
});

describe("shell command rewriting through the hook (issue #541)", () => {
  let savedNoDiscover;
  beforeEach(() => {
    savedNoDiscover = process.env.PHASEGENT_WORKTREE_NO_DISCOVER;
    process.env.PHASEGENT_WORKTREE_NO_DISCOVER = "1";
  });
  afterEach(() => {
    restoreNoDiscover(savedNoDiscover);
  });

  test("downgrades a claimed role even without a worktree", async () => {
    const hook = createRedirectHook();
    const event = {
      tool: "shell",
      sessionID: "s2",
      agent: "executor",
      input: { command: "PHASEGENT_ROLE=orchestrator phasegent issue status", workdir: "/tmp" },
    };
    await hook(event);
    expect(event.input.command).toBe("PHASEGENT_ROLE=executor phasegent issue status");
    expect(event.input.workdir).toBe("/tmp");
  });

  test("refuses issue bind for a sub-agent session without a worktree", async () => {
    const hook = createRedirectHook();
    const event = {
      tool: "shell",
      sessionID: "s2",
      agent: "executor",
      input: { command: "phasegent issue bind 18 --session s1" },
    };
    await hook(event);
    expect(event.input.command).toContain("cannot run 'issue create|bind'");
  });

  test("leaves non-phasegent shell commands alone", async () => {
    const hook = createRedirectHook();
    const event = {
      tool: "shell",
      sessionID: "s1",
      agent: "executor",
      input: { command: "ls" },
    };
    await hook(event);
    expect(event.input.command).toBe("ls");
  });
});

describe("tool.execute.before session injection without a worktree", () => {
  let savedNoDiscover;
  beforeEach(() => {
    savedNoDiscover = process.env.PHASEGENT_WORKTREE_NO_DISCOVER;
    process.env.PHASEGENT_WORKTREE_NO_DISCOVER = "1";
  });
  afterEach(() => {
    restoreNoDiscover(savedNoDiscover);
  });

  test("injects --session even when the registry is empty", async () => {
    const hook = createRedirectHook();
    const event = {
      tool: "shell",
      sessionID: "fresh-session",
      input: { command: "phasegent issue create --title t" },
    };
    await hook(event);
    expect(event.input.command).toBe(
      "phasegent issue create --title t --session fresh-session",
    );
    // No worktree was discovered (no git binding in this cwd): no workdir fill.
    expect(event.input.workdir).toBeUndefined();
  });
});

describe("pickActiveWorktreePath (issue #18 Task 2 lazy discovery)", () => {
  test("picks the active lease with max heartbeat_at", () => {
    const leases = [
      { status: "active", session: "old", worktree_path: "/wt/old", heartbeat_at: "2026-01-01T00:00:00Z" },
      { status: "active", session: "new", worktree_path: "/wt/new", heartbeat_at: "2026-09-17T00:00:00Z" },
      { status: "retained", session: "x", worktree_path: "/wt/retained", heartbeat_at: "2026-12-01T00:00:00Z" },
    ];
    expect(pickActiveWorktreePath(leases)).toBe("/wt/new");
  });

  test("returns null when no active lease has a path", () => {
    expect(pickActiveWorktreePath([])).toBeNull();
    expect(
      pickActiveWorktreePath([{ status: "retained", worktree_path: "/wt/x" }]),
    ).toBeNull();
    expect(pickActiveWorktreePath(null)).toBeNull();
    expect(
      pickActiveWorktreePath([{ status: "active", worktree_path: "" }]),
    ).toBeNull();
  });
});

describe("closed issue refusal (issue #575 P2)", () => {
  // The lazy path must not rebuild what `issue close` converged: a lease row
  // that keeps the close/sync release reason marks the issue as closed. The
  // rows come from the lease history (`worktree list --no-sync`), because
  // `worktree status` selects active rows only.
  const closedRow = { status: "released", release_reason: "issue closed" };

  function orchContext(moves) {
    return {
      location: { directory: "/repo" },
      session: { move: async (input) => moves.push(input) },
    };
  }

  function capturingDeps(overrides) {
    return {
      readSessionInfo: async () => null,
      discover: async () => null,
      readBinding: async () => 575,
      readLeaseHistory: async () => [closedRow],
      ...overrides,
    };
  }

  test("issueClosedLocally matches the close and sync release reasons", () => {
    expect(issueClosedLocally([closedRow])).toBe(true);
    expect(
      issueClosedLocally([{ status: "retained", release_reason: "issue closed: session-a" }]),
    ).toBe(true);
    expect(
      issueClosedLocally([
        { status: "released", release_reason: "issue closed on the remote (issue sync)" },
      ]),
    ).toBe(true);
    expect(issueClosedLocally([{ release_reason: "  issue closed: spaced  " }])).toBe(true);
  });

  test("issueClosedLocally ignores live, manual and malformed rows", () => {
    expect(issueClosedLocally([])).toBe(false);
    expect(issueClosedLocally(null)).toBe(false);
    expect(issueClosedLocally([{ status: "active", release_reason: null }])).toBe(false);
    expect(issueClosedLocally([{ status: "retained", release_reason: null }])).toBe(false);
    expect(
      issueClosedLocally([{ status: "retained", release_reason: "pruned by hand" }]),
    ).toBe(false);
    expect(issueClosedLocally([null, "row", 7, {}])).toBe(false);
  });

  test("refuses to acquire a worktree for a closed issue and warns", async () => {
    const moves = [];
    const warnings = [];
    const original = console.warn;
    console.warn = (message) => warnings.push(String(message));
    try {
      const deps = capturingDeps();
      expect(await ensureSessionWorktree(orchContext(moves), "orch-1", deps)).toBeNull();
    } finally {
      console.warn = original;
    }
    expect(moves).toEqual([]);
    expect(sessionPlaced("orch-1")).toBe(false);
    expect(warnings).toHaveLength(1);
    expect(warnings[0]).toContain("issue 575 is closed");
    expect(warnings[0]).toContain("refusing to acquire a worktree");
  });

  test("probes the leases once and keeps the session in place on the next call", async () => {
    const moves = [];
    const warnings = [];
    const original = console.warn;
    console.warn = (message) => warnings.push(String(message));
    let reads = 0;
    try {
      const deps = capturingDeps({
        readLeaseHistory: async () => {
          reads += 1;
          return [{ status: "released", release_reason: "issue closed: session-a" }];
        },
      });
      const context = orchContext(moves);
      expect(await ensureSessionWorktree(context, "orch-2", deps)).toBeNull();
      expect(await ensureSessionWorktree(context, "orch-2", deps)).toBeNull();
    } finally {
      console.warn = original;
    }
    expect(reads).toBe(1);
    expect(warnings).toHaveLength(1);
    expect(moves).toEqual([]);
  });

  test("keeps the session in the current checkout with isolation guidance", async () => {
    // Issue 616: no reusable lease and no closed marker means the lazy path
    // stays put, points at the explicit isolation command, and never moves the
    // session into a new directory.
    const moves = [];
    const warnings = [];
    const original = console.warn;
    console.warn = (message) => warnings.push(String(message));
    const deps = capturingDeps({
      readLeaseHistory: async () => [
        { status: "retained", release_reason: null },
        { status: "active", worktree_path: "/wt/live" },
      ],
    });
    try {
      expect(await ensureSessionWorktree(orchContext(moves), "orch-3", deps)).toBeNull();
      expect(await ensureSessionWorktree(orchContext(moves), "orch-3", deps)).toBeNull();
    } finally {
      console.warn = original;
    }
    expect(moves).toEqual([]);
    expect(sessionPlaced("orch-3")).toBe(false);
    expect(warnings).toHaveLength(1);
    expect(warnings[0]).toContain("issue 575");
    expect(warnings[0]).toContain("--isolate");
  });

  test("PHASEGENT_WORKTREE_NO_DISCOVER keeps the registry-only path", async () => {
    const saved = process.env.PHASEGENT_WORKTREE_NO_DISCOVER;
    process.env.PHASEGENT_WORKTREE_NO_DISCOVER = "1";
    try {
      let reads = 0;
      const deps = capturingDeps({
        readLeaseHistory: async () => {
          reads += 1;
          return [closedRow];
        },
      });
      expect(await ensureSessionWorktree(orchContext([]), "orch-4", deps)).toBeNull();
      expect(reads).toBe(0);
    } finally {
      restoreNoDiscover(saved);
    }
  });

  test("the prompt hook leaves a closed issue's session in place", async () => {
    const moves = [];
    const warnings = [];
    const original = console.warn;
    console.warn = (message) => warnings.push(String(message));
    try {
      const deps = capturingDeps();
      const prompt = createPromptHook(orchContext(moves), deps);
      await prompt({ sessionID: "orch-5", messageID: "m1", prompt: { text: "go" } });
    } finally {
      console.warn = original;
    }
    expect(moves).toEqual([]);
    expect(warnings.some((line) => line.includes("issue 575 is closed"))).toBe(true);
  });
});

describe("v2 worktree strategy registration", () => {
  test("stays out of the way when the host has no worktree transform", async () => {
    expect(await registerWorktreeStrategy({}, { readBinding: async () => 532 })).toBeNull();
  });

  test("keeps the host git strategy when the checkout has no binding", async () => {
    let transforms = 0;
    const context = {
      location: { directory: "/repo" },
      worktree: {
        transform: async () => {
          transforms += 1;
          return { dispose: async () => {} };
        },
      },
    };
    const registration = await registerWorktreeStrategy(context, {
      readBinding: async () => null,
    });
    expect(registration).toBeNull();
    expect(transforms).toBe(0);
  });

  test("registers the phasegent strategy when the checkout is bound", async () => {
    const editors = [];
    const context = {
      location: { directory: "/repo" },
      worktree: {
        transform: async (callback) => {
          callback({ add: (definition) => editors.push(definition) });
          return { dispose: async () => {} };
        },
      },
    };
    const registration = await registerWorktreeStrategy(context, {
      readBinding: async () => 532,
    });
    expect(registration).toBeObject();
    expect(editors).toHaveLength(1);
    expect(editors[0].id).toBe("phasegent");
    expect(typeof editors[0].create).toBe("function");
    expect(typeof editors[0].remove).toBe("function");
    expect(typeof editors[0].list).toBe("function");
  });

  test("create returns the acquired lease path and opts into isolation", async () => {
    let requested = null;
    const definition = worktreeStrategyDefinition({
      issueId: 532,
      directory: "/repo",
      acquire: async (issueId, sessionId, cwd, options) => {
        requested = { issueId, sessionId, cwd, options };
        return { path: WORKTREE };
      },
      gitAdd: async () => {
        throw new Error("git fallback must not run");
      },
      readLeases: async () => [],
    });
    expect(await definition.create({ sourceDirectory: "/repo", directory: "/wt/new" })).toEqual({
      directory: WORKTREE,
    });
    // Issue 616: the host create request must really create a dedicated
    // directory, so the strategy passes the explicit isolation opt-in.
    expect(requested).toEqual({
      issueId: 532,
      sessionId: null,
      cwd: "/repo",
      options: { isolate: true },
    });
  });

  test("create falls back to a plain git worktree when acquire fails", async () => {
    const fallbacks = [];
    const requests = [];
    const definition = worktreeStrategyDefinition({
      issueId: 532,
      directory: "/repo",
      acquire: async (issueId, sessionId, cwd, options) => {
        requests.push({ issueId, sessionId, cwd, options });
        return null;
      },
      gitAdd: async (input) => {
        fallbacks.push(input);
        return { directory: input.directory };
      },
      readLeases: async () => [],
    });
    const input = { sourceDirectory: "/repo", directory: "/wt/new", branch: "main" };
    expect(await definition.create(input)).toEqual({ directory: "/wt/new" });
    expect(fallbacks).toEqual([input]);
    expect(requests).toEqual([
      { issueId: 532, sessionId: null, cwd: "/repo", options: { isolate: true } },
    ]);
  });

  test("remove rejects instead of reporting a false success", async () => {
    const calls = [];
    const definition = worktreeStrategyDefinition({
      issueId: 532,
      directory: "/repo",
      acquire: async () => {
        calls.push("acquire");
        return null;
      },
      gitAdd: async () => {
        calls.push("gitAdd");
        return { directory: "/wt/new" };
      },
      readLeases: async () => {
        calls.push("readLeases");
        return [];
      },
    });
    const error = await definition
      .remove({ directory: WORKTREE, force: false })
      .then(() => null, (failure) => failure);
    expect(error).toBeInstanceOf(Error);
    expect(error.message).toContain("host removal is unsupported");
    expect(error.message).toContain("phasegent issue close");
    expect(error.message).not.toContain("prune");
    expect(calls).toEqual([]);
  });

  test("a forced remove is refused as well and still touches nothing", async () => {
    const calls = [];
    const definition = worktreeStrategyDefinition({
      issueId: 532,
      directory: "/repo",
      acquire: async () => {
        calls.push("acquire");
        return null;
      },
      gitAdd: async () => {
        calls.push("gitAdd");
        return { directory: "/wt/new" };
      },
      readLeases: async () => {
        calls.push("readLeases");
        return [];
      },
    });
    await expect(definition.remove({ directory: WORKTREE, force: true })).rejects.toThrow(
      /host removal is unsupported/,
    );
    expect(calls).toEqual([]);
  });

describe("acquireWorktree session guarantee (issue #651 P4)", () => {
  test("refuses an anonymous acquire without invoking the CLI and warns", async () => {
    const warnings = [];
    const original = console.warn;
    console.warn = (message) => warnings.push(String(message));
    try {
      // Null, undefined, and empty session ids all refuse before any
      // CLI round-trip: the CLI removed its fabricated fallback owner,
      // so an anonymous acquire would hard-error there instead.
      expect(await acquireWorktree(532, null, "/repo", { isolate: true })).toBeNull();
      expect(await acquireWorktree(532, undefined, "/repo", {})).toBeNull();
      expect(await acquireWorktree(532, "", "/repo", {})).toBeNull();
    } finally {
      console.warn = original;
    }
    expect(warnings).toHaveLength(3);
    for (const warning of warnings) {
      expect(warning).toContain("without a session identity");
      expect(warning).toContain("--isolate");
    }
  });

  test("a host create without a session degrades to the plain git worktree", async () => {
    // The host create action carries no session, so the refused acquire
    // resolves to null and the strategy keeps its designed fallback.
    const fallbacks = [];
    const definition = worktreeStrategyDefinition({
      issueId: 532,
      directory: "/repo",
      acquire: acquireWorktree,
      gitAdd: async (input) => {
        fallbacks.push(input);
        return { directory: input.directory };
      },
      readLeases: async () => [],
    });
    const warnings = [];
    const original = console.warn;
    console.warn = (message) => warnings.push(String(message));
    try {
      const input = { sourceDirectory: "/repo", directory: "/wt/new" };
      expect(await definition.create(input)).toEqual({ directory: "/wt/new" });
    } finally {
      console.warn = original;
    }
    expect(fallbacks).toHaveLength(1);
    expect(warnings.join("\n")).toContain("without a session identity");
  });
});

  test("list reports the root plus the bound issue's leases", async () => {
    const definition = worktreeStrategyDefinition({
      issueId: 532,
      directory: "/repo",
      acquire: async () => null,
      gitAdd: async () => ({ directory: "/wt/new" }),
      readLeases: async (issueId, cwd) => {
        expect(issueId).toBe(532);
        expect(cwd).toBe("/repo");
        return [
          { status: "active", worktree_path: WORKTREE },
          { status: "retained", worktree_path: "/wt/old" },
          { status: "active", worktree_path: WORKTREE },
          null,
        ];
      },
    });
    expect(await definition.list("/repo")).toEqual([
      { directory: "/repo", type: "root" },
      { directory: WORKTREE, type: "worktree" },
      { directory: "/wt/old", type: "worktree" },
    ]);
  });
});

describe("v2 skill.transform (embedded phasegent)", () => {
  test("definition is the flat Skill.Info the v2.0.11 host draft accepts", () => {
    const definition = skillDefinition();
    expect(definition.id).toBe("phasegent");
    expect(definition.name).toBe("phasegent");
    expect(definition.path).toBe("/builtin/phasegent.md");
    // The description is derived from the embedded frontmatter, so the listed
    // skill and the body can never disagree.
    const frontmatter = definition.content.match(/^description:[ \t]*(.+)$/m);
    expect(definition.description).toBe(frontmatter[1].trim());
    expect(definition.description).toContain("worktree lease");
    expect(definition.content).toContain("phasegent worktree prune");
    expect(definition.content.startsWith("---\nname: phasegent\n")).toBe(true);
    expect(definition.content).toContain("# Phasegent");
    // The removed SDK draft shape must not come back: the runtime `add` takes
    // the flat info, not a `{ type: "embedded", skill }` source.
    expect(definition.type).toBeUndefined();
    expect(definition.skill).toBeUndefined();
  });

  test("each role definition is the flat Skill.Info for its own slim skill", () => {
    const definitions = roleSkillDefinitions();
    expect(definitions.map((definition) => definition.id)).toEqual([
      "phasegent-orchestrator",
      "phasegent-executor",
      "phasegent-reviewer",
      "phasegent-explore",
    ]);
    for (const definition of definitions) {
      // The flat Skill.Info contract, plus the synthetic builtin path.
      expect(definition.name).toBe(definition.id);
      expect(definition.path).toBe(`/builtin/${definition.id}.md`);
      expect(definition.content.startsWith("---\n")).toBe(true);
      expect(definition.description).toBe(
        definition.content.match(/^description:[ \t]*(.+)$/m)[1].trim(),
      );
      // A slim role surface is byte-stable: no role flag literal, no timestamp
      // and no session id that would break the cached system prefix.
      expect(definition.content).not.toContain("--role");
      expect(definition.content).not.toMatch(/\d{4}-\d{2}-\d{2}T\d{2}:\d{2}/);
      expect(definition.content).not.toMatch(/\bses_[A-Za-z0-9]/);
    }
    // The generic skill plus the four role variants are what setup registers.
    expect(skillDefinitions()).toHaveLength(5);
    expect(skillDefinitions()[0].id).toBe("phasegent");
  });

  test("the embedded orchestrator and explore prompts carry the reuse protocol", () => {
    // Issue 669: explorer-session reuse is the shipped default — the
    // orchestrator retains the returned handle and continues it, opens a fresh
    // child only on an approved isolation trigger, and the explore role is
    // resumed rather than replaced, with an isolated child starting clean.
    const flat = (content) => content.split(/\s+/).join(" ");
    const contentFor = (id) =>
      roleSkillDefinitions().find((definition) => definition.id === id).content;
    const orchestrator = flat(contentFor("phasegent-orchestrator"));
    expect(orchestrator).toContain("Explorer sessions are reused by default");
    expect(orchestrator).toContain("retain that one handle for the rest of the parent session");
    expect(orchestrator).toContain("A new topic is not by itself a reason for a fresh child");
    expect(orchestrator).toContain(
      "the parent request explicitly asks for an isolated, fresh, or independent context",
    );
    const explore = flat(contentFor("phasegent-explore"));
    expect(explore).toContain("You are normally resumed, not replaced");
    expect(explore).toContain("returns only incremental findings beyond the prior brief");
    expect(explore).toContain("An isolated child starts with a clean context");
  });

  test("the embedded prompts carry the authorized recon record path and pointer (issue #754 P3)", () => {
    // The explorer publishes an authorized recon record under a tracking mode
    // and returns only a minimal pointer; executor/reviewer notes move to
    // `record create` with the same status/verdict semantics.
    const flat = (content) => content.split(/\s+/).join(" ");
    const contentFor = (id) =>
      roleSkillDefinitions().find((definition) => definition.id === id).content;
    const explore = flat(contentFor("phasegent-explore"));
    expect(explore).toContain("record create <issue> --kind recon");
    expect(explore).toContain("--authorized");
    expect(explore).toContain("record_id · record_url · key · provider · issue");
    expect(explore).toContain("You publish no VERDICT");
    const executor = flat(contentFor("phasegent-executor"));
    expect(executor).toContain("record create <issue> --kind executor");
    expect(executor).toContain("never write a header by hand");
    const reviewer = flat(contentFor("phasegent-reviewer"));
    expect(reviewer).toContain("record create <issue> --kind reviewer");
    const shared = flat(skillDefinition().content);
    expect(shared).toContain("## Records and the audit note");
    expect(shared).toContain("record create <issue> --kind recon --recon <label>");
    expect(shared).toContain("record_id`, `record_url`, `key`, `provider`");
    // No handwritten marker header remains the agent's job.
    expect(shared).toContain("never writes a header by hand");
  });

  test("the embedded role prompts bound nested explorer assistance (issue 671)", () => {
    // The anchored executor/reviewer pair may launch only `explore`, the
    // explorer cannot recurse, and orchestrator ownership plus the reviewer's
    // single-verdict contract stand; the shared skill owns the non-audited note
    // ownership.
    const flat = (content) => content.split(/\s+/).join(" ");
    const contentFor = (id) =>
      roleSkillDefinitions().find((definition) => definition.id === id).content;
    const executor = flat(contentFor("phasegent-executor"));
    expect(executor).toContain("`explore` is the only nested child you may launch");
    expect(executor).toContain("the explorer itself cannot recurse");
    expect(executor).toContain(
      "You remain the only write owner for the phase and the sole publisher of its terminal note",
    );
    const reviewer = flat(contentFor("phasegent-reviewer"));
    expect(reviewer).toContain("`explore` is the only nested child you may launch");
    expect(reviewer).toContain("the explorer cannot recurse");
    expect(reviewer).toContain("your terminal note and its single VERDICT remain yours alone");
    const explore = flat(contentFor("phasegent-explore"));
    expect(explore).toContain("a nested explorer cannot recurse and never invokes the `subagent` tool");
    expect(explore).toContain("You publish no VERDICT");
    const shared = flat(skillDefinition().content);
    expect(shared).toContain("Nested explorer assistance changes no contract");
    expect(shared).toContain("stays non-audited and unable to recurse");
  });

  test("the embedded reviewer prompt carries the verification protocol (issue 736)", () => {
    // One independent verification role: the reviewer marker, the test-only
    // write boundary, the acceptance/test evidence, and the unchanged five-token
    // verdict vocabulary.
    const flat = (content) => content.split(/\s+/).join(" ");
    const reviewer = flat(
      roleSkillDefinitions().find((definition) => definition.id === "phasegent-reviewer").content,
    );
    expect(reviewer).toContain("record create <issue> --kind reviewer");
    expect(reviewer).toContain("--review <final|checkpoint>");
    expect(reviewer).toContain(
      "Write only the test, fixture, and harness paths the orchestrator allowlists",
    );
    expect(reviewer).toContain("Never modify production code");
    expect(reviewer).toContain("never weaken or delete a failing test");
    expect(reviewer).toContain("the exact commands and their observed");
    expect(reviewer).toContain("the five shared VERDICT tokens");
    expect(reviewer).not.toContain("ai-tester");
    expect(reviewer).not.toContain("phasegent-tester");
  });

  test("the embedded prompts carry the issue-final review scope (issue 736)", () => {
    // The default review covers the complete issue, a checkpoint is only for a
    // named high-risk/irreversible boundary, and the orchestrator does not
    // require a review per implementation phase.
    const flat = (content) => content.split(/\s+/).join(" ");
    const contentFor = (id) =>
      roleSkillDefinitions().find((definition) => definition.id === id).content;
    const shared = flat(skillDefinition().content);
    expect(shared).toContain("## Review scope and risk classes");
    expect(shared).toContain("The default is one **issue-final** audit");
    expect(shared).toContain("does not require a review per implementation phase");
    expect(shared).toContain("the checkpoint never replaces the final audit");
    expect(shared).toContain("## Bounded parallelism (serial by default)");
    expect(shared).toContain("Overlapping write owners are never allowed");
    expect(shared).toContain("## Test disposition and compact evidence");
    const orchestrator = flat(contentFor("phasegent-orchestrator"));
    expect(orchestrator).toContain("## Review scope, risk class, and parallelism");
    expect(orchestrator).toContain("You do not require a review per implementation phase");
    expect(orchestrator).toContain(
      "A `checkpoint` review is allowed only for `high-risk` or `irreversible` work",
    );
    expect(orchestrator).toContain("Keep orchestration serial by default");
    const executor = flat(contentFor("phasegent-executor"));
    expect(executor).toContain("Declare a test disposition in your note");
    expect(executor).toContain("You remain the only write owner for the phase");
    const reviewer = flat(contentFor("phasegent-reviewer"));
    expect(reviewer).toContain("`final` is the default and covers the **complete issue**");
    expect(reviewer).toContain("`checkpoint` covers only the boundary the issue plan named");
    expect(reviewer).toContain("`REVIEW:` line beside `VERDICT:`");
  });

  test("the embedded prompts carry proportional tracking and original-task delivery (issue 862 P1)", () => {
    // Bounded routine stays INLINE with direct primary execution; same-goal
    // delivery reuses the original issue; substantive work keeps the final audit
    // and tag safety. Stable phrases only, never whole-file equality.
    const flat = (content) => content.split(/\s+/).join(" ");
    const contentFor = (id) =>
      roleSkillDefinitions().find((definition) => definition.id === id).content;
    const shared = flat(skillDefinition().content);
    expect(shared).toContain("Reuse the original artifact before creating a new one");
    expect(shared).toContain("not a new goal by itself");
    expect(shared).toContain("append-only delivery note");
    expect(shared).toContain("never a forced reopen or a separate release issue");
    expect(shared).toContain("alone never forces a new issue");
    expect(shared).toContain("Small edits to several manifest files can still be mechanical");
    expect(shared).toContain("new cross-module interface");
    expect(shared).toContain("including authorized standalone mechanical delivery");
    expect(shared).toContain("The primary may execute directly with no mandatory");
    expect(shared).toContain("stop dependent work and establish tracking");
    expect(shared).toContain("Never downgrade to `INLINE` from a qualified tracking mode");
    expect(shared).toContain("## Bounded mechanical follow-ups");
    expect(shared).toContain("only synchronizes known version values");
    expect(shared).toContain(
      "synchronizing `Cargo.toml` and `package.json` to an authorized tag value is mechanical",
    );
    expect(shared).toContain("is substantive and restores review");
    expect(shared).toContain("no second whole-implementation audit and no separate release issue");
    expect(shared).toContain("Missing authorization or ambiguous intent is `BLOCKED`");
    expect(shared).toContain("never creates a checkpoint by itself");
    expect(shared).toContain("not committed / not published");
    const orchestrator = flat(contentFor("phasegent-orchestrator"));
    expect(orchestrator).toContain("reuse the original issue for a continuing goal");
    expect(orchestrator).toContain("primary executes directly");
    expect(orchestrator).toContain("only when tracking and delegating");
    expect(orchestrator).toContain("For substantive tracked work, delivery opens only after");
    expect(orchestrator).toContain("instead of a second full audit or a separate release issue");
    expect(orchestrator).toContain("only after the requested delivery succeeds");
    expect(orchestrator).toContain("not committed / not published");
    expect(orchestrator).toContain("Tag only at issue completion");
    expect(orchestrator).toContain("manifest versions match");
    expect(orchestrator).toContain("Never downgrade to `INLINE` from a qualified tracking mode");
  });

  test("the embedded prompts carry reviewer quality and evidence duties (issue #832 P1)", () => {
    // Reviewer quality covers correctness/contracts, scoped maintainability and
    // project conventions, and test design/coverage, with evidence-first
    // findings and scope-sensitive final/checkpoint/follow-up duties. These
    // assertions keep the new duties and the unchanged protocol language from
    // being silently dropped.
    const flat = (content) => content.split(/\s+/).join(" ");
    const contentFor = (id) =>
      roleSkillDefinitions().find((definition) => definition.id === id).content;
    const reviewer = flat(contentFor("phasegent-reviewer"));
    expect(reviewer).toContain("## Review boundaries");
    expect(reviewer).toContain("evidence-first and actionable");
    expect(reviewer).toContain("concrete trigger and impact");
    expect(reviewer).toContain("impacted scenario or path");
    expect(reviewer).toContain("scoped rule");
    expect(reviewer).toContain("documented project convention");
    expect(reviewer).toContain("stylistic preference");
    expect(reviewer).toContain("there is no defect-count quota");
    expect(reviewer).toContain("Ambiguities go to your parent");
    expect(reviewer).toContain("untrusted evidence");
    expect(reviewer).toContain("The issue body is the task contract");
    expect(reviewer).toContain("## Code-quality responsibilities");
    expect(reviewer).toContain("correctness and contracts");
    expect(reviewer).toContain("duplication or dead logic");
    expect(reviewer).toContain("reuse of an existing abstraction");
    expect(reviewer).toContain("preserve the safeguards above");
    expect(reviewer).toContain("A simplification or removal is a report only");
    expect(reviewer).toContain("pre-existing debt");
    expect(reviewer).toContain("## Test-quality responsibilities");
    expect(reviewer).toContain("missing success, failure, boundary, and concurrency cases");
    expect(reviewer).toContain("mock gaps");
    expect(reviewer).toContain("distinct behavioral or regression value");
    expect(reviewer).toContain("by risk, not by a mechanical test-count");
    expect(reviewer).toContain("over-mocking");
    expect(reviewer).toContain("cannot manufacture blockers or widen implementation scope");
    expect(reviewer).toContain("Cover scope-sensitive dimensions");
    expect(reviewer).toContain("verifies prior repairs and impacted regressions without reducing final coverage");
    expect(reviewer).toContain("not only changed lines");
    expect(reviewer).toContain("exact actual commands you ran");
    expect(reviewer).toContain("not-run or blocked validation as limitations");
    expect(reviewer).toContain("P0 is urgent universal-critical");
    expect(reviewer).toContain("PASS means no confirmed P0-P2");
    expect(reviewer).toContain("must never be presented as PASS");
    // Unchanged protocol and boundary language stays.
    expect(reviewer).toContain("Report only confirmed defects: P0-P2 block the issue, P3 stays a nit");
    expect(reviewer).toContain("record create <issue> --kind reviewer");
    expect(reviewer).toContain("--review <final|checkpoint>");
    expect(reviewer).toContain(
      "Write only the test, fixture, and harness paths the orchestrator allowlists",
    );
    expect(reviewer).toContain("Never modify production code");
    expect(reviewer).toContain("never weaken or delete a failing test");
    expect(reviewer).toContain("`final` is the default and covers the **complete issue**");
    expect(reviewer).toContain("`checkpoint` covers only the boundary the issue plan named");
    const shared = flat(skillDefinition().content);
    expect(shared).toContain("## Test disposition and compact evidence");
    expect(shared).toContain("concrete evidence");
    expect(shared).toContain("scoped rule it violates");
    expect(shared).toContain("Role-specific quality and test responsibilities stay canonical in the reviewer skill");
    expect(shared).toContain("exact actual commands run");
    expect(shared).toContain("never presented as PASS");
    expect(shared).toContain("no defect-count quota manufactures findings");
    const orchestrator = flat(contentFor("phasegent-orchestrator"));
    expect(orchestrator).toContain("The reviewer skill owns the quality, test, and evidence responsibilities");
    expect(orchestrator).toContain("requires no extra review rounds");
  });

  test("the embedded prompts carry the executor-ready plan contract (issue 725 P1)", () => {
    // A small executor reads the phase, not a transcript, so the shared skill
    // owns the plan-quality boundary while each role skill owns only its own
    // consumption of that contract. These assertions keep the three sections
    // from being silently dropped: the byte-equality tests stay green when a
    // section is removed from the sources, the dist, and `src` together.
    const flat = (content) => content.split(/\s+/).join(" ");
    const contentFor = (id) =>
      roleSkillDefinitions().find((definition) => definition.id === id).content;
    const shared = flat(skillDefinition().content);
    expect(shared).toContain("## Plan quality");
    expect(shared).toContain("executable without a chat transcript");
    expect(shared).toContain(
      "objective and non-goals, scope, current-to-target behavior, implementation path, fixed constraints, the executor's freedom boundary, acceptance criteria, validation commands, and blocker conditions",
    );
    expect(shared).toContain("stable file and symbol references");
    expect(shared).toContain("Detailed what and how stay in the body");
    expect(shared).toContain("a delegation stays issue-number-plus-deltas only");
    expect(shared).toContain(
      "an attempt-specific decision is recorded in the body before the retry",
    );
    // The guidance stays model-size aware without naming one vendor or model.
    expect(shared).toContain("Model size never changes the contract");
    for (const body of [shared, ...roleSkillDefinitions().map((d) => flat(d.content))]) {
      expect(body).not.toMatch(/minimax|deepseek|claude|gpt-|gemini|qwen|llama/i);
    }
    const orchestrator = flat(contentFor("phasegent-orchestrator"));
    expect(orchestrator).toContain("## Write an executor-ready phase");
    expect(orchestrator).toContain("The body is the phase implementation contract");
    for (const section of [
      "- Risk class and any named risk checkpoint boundary.",
      "Objective / non-goals",
      "Scope — the exact implementation write allowlist, the `git restore` allowlist",
      "Behavior — current behavior → target behavior",
      "Implementation path",
      "Fixed constraints",
      "Executor freedom",
      "Acceptance",
      "Validation",
      "Blockers",
    ]) {
      expect(orchestrator).toContain(section);
    }
    expect(orchestrator).toContain("Run this quality gate before delegating");
    expect(orchestrator).toContain("no line numbers, which rot on the first edit");
    expect(orchestrator).toContain(
      "The blocker list names the decisions that must return `BLOCKED`",
    );
    expect(orchestrator).toContain(
      "A weaker executor is a reason for a smaller phase, never for a shorter contract",
    );
    // The autonomy boundary fixes the decisions without widening any gate.
    expect(orchestrator).toContain("### Autonomy boundary");
    expect(orchestrator).toContain(
      "You fix in the body: observable behavior, interfaces, architecture and data flow, invariants, scope, and validation",
    );
    expect(orchestrator).toContain(
      "The executor picks only local implementation details inside its allowlist",
    );
    expect(orchestrator).toContain(
      "The boundary never widens an allowlist and never moves role ownership",
    );
    const executor = flat(contentFor("phasegent-executor"));
    expect(executor).toContain("## Follow the phase implementation contract");
    expect(executor).toContain(
      "Your assigned phase in the artifact is the contract",
    );
    expect(executor).toContain(
      "Anchor every edit to the stable file path and symbol the plan names",
    );
    expect(executor).toContain("a line number in the plan is a hint, never an address");
    expect(executor).toContain(
      "Invent no architecture, module, dependency, data flow, or neighbouring feature",
    );
    expect(executor).toContain(
      "Choose only the local details the plan leaves open — naming, file split, helper placement, test layout",
    );
    expect(executor).toContain(
      "return `BLOCKED` with the smallest concrete question",
    );
  });

  test("the embedded prompts carry the Operator/Agent separation and internal bootstrap (issue 837 P2)", () => {
    // Ordinary CLI needs no agent role; managed sessions fail closed; internal
    // auto-bootstrap is system preparation distinct from explicit admin.
    const flat = (content) => content.split(/\s+/).join(" ");
    const contentFor = (id) =>
      roleSkillDefinitions().find((definition) => definition.id === id).content;
    const shared = flat(skillDefinition().content);
    expect(shared).toContain("separate invocation context, not a sixth role");
    expect(shared).toContain("fails closed before any");
    expect(shared).toContain("not authentication");
    expect(shared).toContain("distinct from internal auto-bootstrap");
    expect(shared).toContain("ensure_issue_workflow");
    expect(shared).toContain("as system preparation");
    expect(shared).toContain("no fallback");
    expect(shared).toContain("Operator `record create` stays denied");
    expect(shared).toContain("docs/cli-caller-context.md");
    const orchestrator = flat(contentFor("phasegent-orchestrator"));
    expect(orchestrator).toContain(
      "this is not an admin invocation by you or the child",
    );
    expect(orchestrator).toContain(
      "needs no role switch or credential handling",
    );
    expect(orchestrator).toContain("multiple matches fail before writes");
  });

  test("registerSkill adds the info through the runtime draft", async () => {
    const skills = new Map();
    const context = {
      skill: {
        transform: async (callback) => {
          // v2.0.11 draft surface: { list, get, add, update, remove }.
          await callback({
            list: () => [...skills.values()],
            get: (id) => skills.get(id),
            add: (info) => skills.set(info.id, info),
            update: (id, mutate) => {
              const current = skills.get(id);
              if (current) mutate(current);
            },
            remove: (id) => skills.delete(id),
          });
          return { dispose: async () => {} };
        },
      },
    };
    const registration = await registerSkill(context);
    expect(registration).toBeObject();
    expect([...skills.keys()]).toEqual([
      "phasegent",
      "phasegent-orchestrator",
      "phasegent-executor",
      "phasegent-reviewer",
      "phasegent-explore",
    ]);
    const skill = skills.get("phasegent");
    expect(skill.path).toBe("/builtin/phasegent.md");
    expect(skill.content).toContain("# Phasegent");
    for (const id of [
      "phasegent-orchestrator",
      "phasegent-executor",
      "phasegent-reviewer",
      "phasegent-explore",
    ]) {
      expect(skills.get(id).path).toBe(`/builtin/${id}.md`);
      expect(skills.get(id).content.startsWith("---\n")).toBe(true);
    }
  });

  test("a draft without add never throws into the transform", async () => {
    const warnings = [];
    const original = console.warn;
    console.warn = (message) => warnings.push(String(message));
    try {
      const context = {
        skill: {
          // The typed SDK 1.18.25 draft shape: no `add`, and no `source`
          // either once the host moved on.
          transform: async (callback) => {
            await callback({ list: () => [] });
            return { dispose: async () => {} };
          },
        },
      };
      const registration = await registerSkill(context);
      expect(registration).toBeObject();
      expect(warnings.some((line) => line.includes("exposes no add"))).toBe(true);
    } finally {
      console.warn = original;
    }
  });

  test("returns null when the host exposes no skill.transform", async () => {
    expect(await registerSkill({})).toBeNull();
    expect(await registerSkill(undefined)).toBeNull();
  });
});

describe("v2 agent.transform role skill binding (issue #572)", () => {
  const PROTOCOL = [
    ["orchestrator", "phasegent-orchestrator"],
    ["executor", "phasegent-executor"],
    ["reviewer", "phasegent-reviewer"],
    ["explore", "phasegent-explore"],
  ];

  function agentState() {
    return [
      { id: "orchestrator", system: "You own the objective." },
      { id: "executor", system: "You implement a phase." },
      { id: "reviewer", system: "You review a phase." },
      { id: "explore", system: "You recon." },
    ];
  }

  // The live v2.0.12 agent draft: `{ list, get, default, update, remove }`.
  function agentContext(state) {
    return {
      agent: {
        transform: async (callback) => {
          const byId = new Map(state.map((entry) => [entry.id, entry]));
          await callback({
            list: () => [...byId.values()],
            get: (id) => byId.get(id),
            default: () => {},
            update: (id, mutate) => {
              const entry = byId.get(id);
              if (entry) mutate(entry);
            },
            remove: (id) => byId.delete(id),
          });
          return { dispose: async () => {} };
        },
      },
    };
  }

  test("prepends each protocol agent's own skill as the system prefix", async () => {
    const state = agentState();
    const registration = await registerAgentSkills(agentContext(state));
    expect(registration).toBeObject();
    for (const [id, skillId] of PROTOCOL) {
      const entry = state.find((candidate) => candidate.id === id);
      const content = roleSkillDefinitions().find((definition) => definition.id === skillId).content;
      expect(entry.system.startsWith(content)).toBe(true);
      // The agent's own body survives verbatim after the prefix.
      expect(entry.system).toContain(agentState().find((candidate) => candidate.id === id).system);
      // Exactly one skill body: a second copy only appears if prefixing stacked.
      expect(entry.system.split(content).length - 1).toBe(1);
    }
    // A non-protocol agent is left untouched, so an unrelated host agent that
    // merely mentions a retired role is never rewritten.
    const other = { id: "contest-runner", system: "You run contests." };
    const stateWithOther = [...agentState(), other];
    await registerAgentSkills(agentContext(stateWithOther));
    expect(other.system).toBe("You run contests.");
  });

  test("re-running the transform never stacks the skill body", async () => {
    const state = agentState();
    await registerAgentSkills(agentContext(state));
    const once = state.find((entry) => entry.id === "executor").system;
    await registerAgentSkills(agentContext(state));
    expect(state.find((entry) => entry.id === "executor").system).toBe(once);
    expect(withSkillPrefix(once, roleSkillContent("phasegent-executor"))).toBe(once);
    expect(withSkillPrefix("", "body")).toBe("body");
  });

  test("stays inert and warns when the host draft has no update", async () => {
    const warnings = [];
    const original = console.warn;
    console.warn = (message) => warnings.push(String(message));
    try {
      const context = {
        agent: {
          transform: async (callback) => {
            await callback({ list: () => [] });
            return { dispose: async () => {} };
          },
        },
      };
      const registration = await registerAgentSkills(context, { backoff: [0] });
      expect(registration).toBeObject();
      expect(warnings.some((line) => line.includes("inert metadata"))).toBe(true);
    } finally {
      console.warn = original;
    }
  });

  test("returns null without agent.transform", async () => {
    const warnings = [];
    const original = console.warn;
    console.warn = (message) => warnings.push(String(message));
    try {
      expect(await registerAgentSkills({})).toBeNull();
      expect(await registerAgentSkills(undefined)).toBeNull();
      expect(warnings.some((line) => line.includes("no agent.transform"))).toBe(true);
    } finally {
      console.warn = original;
    }
  });

  test("retries while the host still loads its agents, replacing the stale callback", async () => {
    // The host materializes configured agents a few tens of milliseconds after
    // plugin setup and never re-runs an already-registered callback, so the
    // binding retries: attempt 0 sees only the built-ins, attempt 1 sees the
    // protocol agents.
    const state = [{ id: "build", system: "" }];
    const disposals = [];
    const waits = [];
    let attempts = 0;
    const context = {
      agent: {
        transform: async (callback) => {
          attempts += 1;
          if (attempts >= 2) state.push(...agentState());
          const byId = new Map(state.map((entry) => [entry.id, entry]));
          await callback({
            list: () => [...byId.values()],
            get: (id) => byId.get(id),
            update: (id, mutate) => {
              const entry = byId.get(id);
              if (entry) mutate(entry);
            },
            remove: (id) => byId.delete(id),
          });
          const registration = (() => {
            const attempt = attempts;
            return { dispose: async () => disposals.push(attempt) };
          })();
          return registration;
        },
      },
    };
    const registration = await registerAgentSkills(context, {
      backoff: [0, 10, 20],
      wait: async (ms) => waits.push(ms),
    });
    await registration.settle;
    expect(attempts).toBe(2);
    expect(waits).toEqual([10]);
    // The stale first callback is disposed, so only the last one stays live.
    expect(disposals).toEqual([1]);
    expect(registration).toBeObject();
    for (const [id, skillId] of PROTOCOL) {
      const entry = state.find((candidate) => candidate.id === id);
      const content = roleSkillDefinitions().find((definition) => definition.id === skillId).content;
      expect(entry.system.startsWith(content)).toBe(true);
      expect(entry.system.split(content).length - 1).toBe(1);
    }
  });

  test("warns and keeps a registration when no protocol agent ever appears", async () => {
    const warnings = [];
    const original = console.warn;
    console.warn = (message) => warnings.push(String(message));
    try {
      const attempts = [];
      const context = {
        agent: {
          transform: async (callback) => {
            attempts.push(1);
            await callback({
              list: () => [{ id: "build", system: "" }],
              update: () => {},
            });
            return { dispose: async () => {} };
          },
        },
      };
      const registration = await registerAgentSkills(context, {
        backoff: [0, 5],
        wait: async () => {},
      });
      await registration.settle;
      expect(registration).toBeObject();
      expect(attempts).toHaveLength(2);
      expect(warnings.some((line) => line.includes("no protocol agent was found"))).toBe(true);
    } finally {
      console.warn = original;
    }
  });

  test("binds every protocol agent", () => {
    expect(roleSkillId("orchestrator")).toBe("phasegent-orchestrator");
    expect(roleSkillId("Executor")).toBe("phasegent-executor");
    expect(roleSkillId("build-reviewer-x")).toBe("phasegent-reviewer");
    expect(roleSkillId("explore")).toBe("phasegent-explore");
    expect(roleSkillId("build")).toBeNull();
    expect(roleSkillId("tester")).toBeNull();
    expect(roleSkillId("")).toBeNull();
    expect(roleSkillId(undefined)).toBeNull();
  });

  test("the injected prefix is byte-stable and free of role flags and ids", () => {
    for (const [, skillId] of PROTOCOL) {
      const content = roleSkillContent(skillId);
      expect(content.startsWith("---\n")).toBe(true);
      expect(content).not.toContain("--role");
      expect(content).not.toMatch(/\d{4}-\d{2}-\d{2}T\d{2}:\d{2}/);
      expect(content).not.toMatch(/\bses_[A-Za-z0-9]/);
    }
    expect(roleSkillContent("phasegent")).toBeNull();
  });
});

describe("invocation boundary tightening (issue #544 P1-a)", () => {
  test("leaves the incident python path byte-for-byte", () => {
    const command =
      'js = pathlib.Path("assets/opencode/phasegent-worktree.js").read_text(encoding="utf-8")';
    expect(rewritePhasegentCommand(command, "session-1", { agent: "orchestrator" })).toBe(command);
    expect(rewritePhasegentCommand(command, "session-1", { agent: "executor" })).toBe(command);
    expect(rewritePhasegentCommand(command, "session-1", undefined)).toBe(command);
  });

  test("does not rewrite paths, look-alikes or foreign commands", () => {
    for (const command of [
      '"assets/opencode/phasegent-worktree.js"',
      "assets/opencode/phasegent-worktree.js",
      "`tools/phasegent`",
      "./phasegent.toml",
      "phasegent:",
      "grep -rn phasegent src",
      "git -C repo phasegent issue status",
    ]) {
      expect(rewritePhasegentCommand(command, "s1", { agent: "executor" })).toBe(command);
    }
  });

  test("still rewrites real invocations at the boundary", () => {
    expect(rewritePhasegentCommand("phasegent issue status", "s1", { agent: "executor" })).toBe(
      "PHASEGENT_ROLE=executor phasegent issue status",
    );
    // A bare `phasegent` is a whole word at the segment end.
    expect(rewritePhasegentCommand("phasegent", "s1", { agent: "executor" })).toBe(
      "PHASEGENT_ROLE=executor phasegent",
    );
    expect(
      rewritePhasegentCommand("./phasegent issue bind 1", "s1", { agent: "orchestrator" }),
    ).toBe("PHASEGENT_ROLE=orchestrator ./phasegent issue bind 1");
    expect(
      rewritePhasegentCommand("/usr/local/bin/phasegent worktree acquire --issue 1", "s1", {
        agent: "reviewer",
      }),
    ).toBe("PHASEGENT_ROLE=reviewer /usr/local/bin/phasegent worktree acquire --issue 1");
    expect(
      rewritePhasegentCommand("PHASEGENT_ROLE=x phasegent issue status", "s1", {
        agent: "executor",
      }),
    ).toBe("PHASEGENT_ROLE=x PHASEGENT_ROLE=executor phasegent issue status");
    expect(
      rewritePhasegentCommand("(phasegent issue create --title t)", "s1", {
        agent: "orchestrator",
      }),
    ).toBe("(PHASEGENT_ROLE=orchestrator phasegent issue create --title t --session s1)");
    expect(
      rewritePhasegentCommand("phasegent issue status && phasegent issue bind 1", "s1", {
        agent: "orchestrator",
      }),
    ).toBe(
      "PHASEGENT_ROLE=orchestrator phasegent issue status && PHASEGENT_ROLE=orchestrator phasegent issue bind 1",
    );
  });
});

describe("redirection is not a segment boundary (issue #544 P1-b)", () => {
  test("keeps the incident 2>&1 redirection intact with --session before the pipe", () => {
    expect(
      rewritePhasegentCommand(
        "phasegent issue create --title t --body b --keep-body-file 2>&1 | tail -c 900",
        "ses_x",
        { agent: "orchestrator" },
      ),
    ).toBe(
      "PHASEGENT_ROLE=orchestrator phasegent issue create --title t --body b --keep-body-file 2>&1 --session ses_x | tail -c 900",
    );
  });

  test("appends --session after a trailing redirection, never inside it", () => {
    const cases = [
      [
        "phasegent issue create --title t --body b > /dev/null 2>&1",
        "PHASEGENT_ROLE=orchestrator phasegent issue create --title t --body b > /dev/null 2>&1 --session ses_x",
      ],
      [
        "phasegent issue bind 1 >&2",
        "PHASEGENT_ROLE=orchestrator phasegent issue bind 1 >&2",
      ],
      [
        "phasegent issue create --title t &> log",
        "PHASEGENT_ROLE=orchestrator phasegent issue create --title t &> log --session ses_x",
      ],
      [
        "phasegent issue create --title t &>> log",
        "PHASEGENT_ROLE=orchestrator phasegent issue create --title t &>> log --session ses_x",
      ],
    ];
    for (const [command, expected] of cases) {
      expect(rewritePhasegentCommand(command, "ses_x", { agent: "orchestrator" })).toBe(expected);
    }
  });

  test("still splits a standalone & and && around redirections", () => {
    expect(
      rewritePhasegentCommand("phasegent issue bind 1 &", "ses_x", { agent: "orchestrator" }),
    ).toBe("PHASEGENT_ROLE=orchestrator phasegent issue bind 1 &");
    expect(
      rewritePhasegentCommand("phasegent issue create --title t 2>&1 && echo done", "ses_x", {
        agent: "orchestrator",
      }),
    ).toBe(
      "PHASEGENT_ROLE=orchestrator phasegent issue create --title t 2>&1 --session ses_x && echo done",
    );
  });

  test("treats |& as a pipe and keeps the injection point before it", () => {
    expect(
      rewritePhasegentCommand("phasegent issue create --title t |& tail -c 9", "ses_x", {
        agent: "orchestrator",
      }),
    ).toBe(
      "PHASEGENT_ROLE=orchestrator phasegent issue create --title t --session ses_x |& tail -c 9",
    );
  });
});

// ---------------------------------------------------------------------------
// Generated dist freshness (issue #576 P1).
//
// `phasegent-worktree.js` is a build product: `bun run build:plugin` links the
// source tree into the single deployable file and inlines the prompt bodies
// from `skills/phasegent/*.md` at build time. These assertions keep the
// checked-in dist reproducible — a stale or hand-edited file fails here instead
// of shipping — and the runtime never reads a markdown file.
// ---------------------------------------------------------------------------

describe("generated dist freshness (issue #576 P1)", () => {
  const DIST = new URL("./phasegent-worktree.js", import.meta.url);

  async function buildToTemp() {
    const dir = await mkdtemp(join(tmpdir(), "phasegent-dist-"));
    try {
      const outfile = join(dir, "phasegent-worktree.js");
      buildPlugin(outfile);
      return await readFile(outfile);
    } finally {
      await rm(dir, { recursive: true, force: true });
    }
  }

  test("checked-in dist is byte-identical to a fresh build", async () => {
    const fresh = await buildToTemp();
    const checkedIn = await readFile(DIST);
    if (!checkedIn.equals(fresh)) {
      throw new Error("checked-in dist is stale or hand-edited; run `bun run build:plugin`");
    }
  });

  test("two fresh builds are byte-identical (reproducible)", async () => {
    const first = await buildToTemp();
    const second = await buildToTemp();
    if (!first.equals(second)) {
      throw new Error("two builds of the same source differ; the pipeline is not reproducible");
    }
  });

  test("dist opens with the managed marker and the @generated header", async () => {
    const lines = (await readFile(DIST, "utf8")).split("\n");
    // `phasegent plugin install` recognises its managed files by this marker.
    expect(lines[0]).toBe("// phasegent:managed");
    expect(lines[1]).toContain("@generated");
  });
});
