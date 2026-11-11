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
import { readIssueDirectories, readRepositoryDirectories } from "./src/binding.js";
import { PROTOCOL_AGENTS } from "./src/agents.js";
import { SKILL_ROUTING_HINT } from "./src/skills.js";

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
  verifiedIssueDirectory,
  verifiedBindingDirectory,
  ensureSessionWorktree,
  readSessionInfo,
  readAuthoritativeDirectory,
  reconcileSessionLocation,
  inheritedWorktree,
  acquireWorktree,
  registerWorktreeStrategy,
  worktreeStrategyDefinition,
  registerAgentSkills,
  isProtocolAgent,
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
  readBinding: async () => ({ issueId: 567, provider: "forgejo", project: "owner/repo", externalId: "567" }),
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

  test("setup registers the tool hook, the prompt hook and agent hint, and returns a cleanup", async () => {
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
    expect(shell.input.command).toBe("phasegent issue get 1");
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
    expect(event.input.command).toBe("phasegent issue get 1");
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
    expect(event.input.command.endsWith("phasegent issue get 1")).toBe(true);
    expect(bridge.size()).toBe(1);
  });

  test("shell hook strips the header and adopts env on the fresh object only", async () => {
    const broker = {
      mint: () => "token-22222222-000000000000000000000000",
      address: () => "127.0.0.1:9999",
      revokeToolCall: () => 0,
    };
    const bridge = createManagedShellBridge({
      randomUUID: () => "22222222-2222-4222-8222-222222222222",
      broker,
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
    expect(shellEvent.command).toBe("phasegent issue get 2");
    expect(shellEvent.env.PHASEGENT_BROKER_ADDR).toBe("127.0.0.1:9999");
    expect(shellEvent.env.PHASEGENT_BROKER_TOKEN).toBe(
      "token-22222222-000000000000000000000000",
    );
    expect(shellEvent.env.PHASEGENT_SESSION_ID).toBe("ses-2");
    expect(process.env.PHASEGENT_SESSION_ID).not.toBe("ses-2");
  });

  test("unknown agent still decorates but never adopts a role", async () => {
    const broker = {
      mint: () => "token-33333333-000000000000000000000000",
      address: () => "127.0.0.1:9999",
      revokeToolCall: () => 0,
    };
    const bridge = createManagedShellBridge({
      randomUUID: () => "33333333-3333-4333-8333-333333333333",
      broker,
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
    // Issue 1154 P1: the parent's origin is the directory the host reports for
    // it, so the pending target stays valid while the host still reports it.
    rememberWorktree("parent-1", WORKTREE, "/wt/host-parent");
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
    // The parent's origin is where the host reports it, so its pending target
    // is what the child follows.
    rememberWorktree("parent-1", WORKTREE, "/repo");
    const sessions = {
      "child-1": { parentID: "parent-1", location: { directory: "/repo" } },
      "parent-1": { parentID: null, location: { directory: "/repo" } },
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
    // The origin is the directory the host reports for the session: the
    // placement is still pending while the host reports it.
    rememberWorktree("session-1", WORKTREE, "/repo");
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
    rememberWorktree("session-1", WORKTREE, "/repo");
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
    rememberWorktree("session-1", WORKTREE, "/repo");
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
    rememberWorktree("session-1", WORKTREE, "/repo");
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

    rememberWorktree("session-1", WORKTREE, "/repo");
    await expect(prompt(promptEvent("session-1"))).resolves.toBeUndefined();
    expect(moves).toHaveLength(2);
  });

  test("resumes an existing child whose parent moved to another worktree", async () => {
    rememberWorktree("parent-1", WORKTREE, "/repo");
    const moves = [];
    // A resumed child still sits in the checkout it was started in, while the
    // parent's registered target is a different worktree.
    const sessions = {
      "child-1": { parentID: "parent-1", location: { directory: "/repo" } },
      "parent-1": { parentID: null, location: { directory: "/repo" } },
    };
    const prompt = createPromptHook(hostContext(sessions, moves), noCliDeps);
    await expect(prompt(promptEvent("child-1"))).resolves.toBeUndefined();
    expect(moves).toEqual([{ sessionID: "child-1", directory: WORKTREE }]);

    // The parent moves again: the child follows on its next prompt.
    const next = "/repo/.worktrees/issue-567";
    resetWorktrees();
    rememberWorktree("parent-1", next, "/repo");
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
    readBinding: async () => ({ issueId: 567, provider: "forgejo", project: "owner/repo", externalId: "567" }),
  };

  test("no session without a reusable directory creates a worktree lazily", async () => {
    // The lazy path never creates a directory. A session with no verified
    // directory stays in the current checkout and gets the explicit isolation
    // guidance once, and the guidance is not repeated per prompt.
    const moves = [];
    const warnings = [];
    const original = console.warn;
    console.warn = (message) => warnings.push(String(message));
    const deps = {
      readSessionInfo: async () => null,
      discover: async () => null,
      readBinding: async () => ({ issueId: 567, provider: "forgejo", project: "owner/repo", externalId: "567" }),
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

  test("a discovered registered directory is reused", async () => {
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
    // The parent's origin is where the host reports it, so the pending target
    // is the one the child follows.
    rememberWorktree("parent-1", WORKTREE, "/wt/host");
    expect(await inheritedWorktree(context, "parent-1")).toBe(WORKTREE);
    // An unregistered parent has no target to keep: the child takes the fresh
    // host location.
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

  test("an unsettled session keeps its target and the first placement still happens", async () => {
    // A session that has not settled yet legitimately sits in the main
    // checkout: that difference is the placement, not a relocation, so it
    // must not cancel the move. Issue 1154 P1: the unsettled session is now
    // reconciled against the host as well, so the target survives only while
    // the host reports the origin it was decided from — and the read is the
    // only evidence that tells the two apart.
    rememberWorktree("session-1", WORKTREE, MAIN);
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
    expect(lookups).toBe(1);
    expect(worktreeForSession("session-1")).toBe(WORKTREE);

    expect(await ensureSessionWorktree(context, "session-1", noCliDeps)).toBe(WORKTREE);
    expect(moves).toEqual([{ sessionID: "session-1", directory: WORKTREE }]);
    expect(sessionPlaced("session-1")).toBe(true);
  });

  test("never reverses a session the close returned to the main checkout", async () => {
    rememberWorktree("session-1", WORKTREE, MAIN);
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

    // The host applied the move, so this prompt confirms the placement.
    await prompt(promptEvent("session-1"));
    expect(moves).toEqual([WORKTREE]);

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
    rememberWorktree("session-1", WORKTREE, MAIN);
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
    // With no cross-session fallback, a new session with no own registration
    // and no parent never borrows another session's worktree.
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
          readBinding: async () => ({ issueId: 747, provider: "forgejo", project: "owner/repo", externalId: "747" }),
        }),
      ).toBeNull();
    });
    expect(moves).toEqual([]);
    expect(warnings.join("\n")).toContain("issue 747");
    expect(warnings.join("\n")).toContain("isolation is opt-in");
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
        readBinding: async () => ({ issueId: 747, provider: "forgejo", project: "owner/repo", externalId: "747" }),
      }),
    ).toBeNull();
    expect(moves).toEqual([]);
  });

  test("a moved session re-registers cleanly on its next issue's directory", async () => {
    // Reconciliation drops the stale target; the ordinary discovery path is
    // untouched, so a later issue re-registers from its own verified directory.
    rememberWorktree("session-1", WORKTREE, MAIN);
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
    // The host applied the move, so this prompt confirms the placement.
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
    // Each root's origin is the directory its own host reports, so the two
    // independent placements never read as each other's relocation.
    rememberWorktree("root-A", rootA, "/audit/repo-A");
    rememberWorktree("root-B", rootB, "/audit/repo-B");
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
    // Parent move admitted but the host has not applied it yet: it still
    // reports the old directory. The child must follow the admitted target, not
    // the lagging host answer.
    rememberWorktree("parent-1", WORKTREE, "/repo");
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
    rememberWorktree("session-1", WORKTREE, "/repo");
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
      rememberWorktree("session-1", WORKTREE, "/repo");
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

// ---------------------------------------------------------------------------
// External relocation reconciliation (issue 1154 P1)
//
// A session can be moved out of band — the OpenCode API, or an operator — and
// the plugin's per-session state never sees it. The host is the authority, so
// a fresh read decides whether a registered target is still the session's
// destination. The origin recorded with the target is what separates the two
// remaining cases: a move that was accepted but has not landed yet keeps the
// session at its origin, while a session the host reports anywhere else was
// relocated and its stale target is dropped.
// ---------------------------------------------------------------------------

describe("external relocation reconciliation (issue 1154 P1)", () => {
  let savedNoDiscover;
  beforeEach(() => {
    savedNoDiscover = process.env.PHASEGENT_WORKTREE_NO_DISCOVER;
    delete process.env.PHASEGENT_WORKTREE_NO_DISCOVER;
  });
  afterEach(() => {
    restoreNoDiscover(savedNoDiscover);
  });

  const MAIN = "/repo";
  const STALE = "/repo/.worktrees/issue-1154";
  const RELOCATED = "/repo/.worktrees/issue-other";

  test("an old unconfirmed target never overrides an externally changed host directory", async () => {
    // The session is registered for the worktree but never settled there, and
    // the host now reports a different directory: the stale target is dropped
    // and the session is not moved back into it.
    rememberWorktree("session-1", STALE, MAIN);
    const moves = [];
    const sessions = {
      "session-1": { parentID: null, location: { directory: RELOCATED } },
    };
    const context = hostContext(sessions, moves);
    const prompt = createPromptHook(context, noCliDeps);

    await expect(prompt(promptEvent("session-1"))).resolves.toBeUndefined();
    expect(moves).toEqual([]);
    expect(worktreeForSession("session-1")).toBeNull();
    expect(sessionPlaced("session-1")).toBe(false);
  });

  test("a third-directory relocation invalidates the parent's registered target", async () => {
    // The parent's registered worktree is stale and the host reports a third
    // directory, so the child follows the host and never the stale target.
    rememberWorktree("parent-1", STALE, MAIN);
    const moves = [];
    const sessions = {
      "parent-1": { parentID: null, location: { directory: RELOCATED } },
      "child-1": { parentID: "parent-1", location: { directory: MAIN } },
    };
    const context = hostContext(sessions, moves);
    const prompt = createPromptHook(context, noCliDeps);

    await expect(prompt(promptEvent("child-1"))).resolves.toBeUndefined();
    expect(moves).toEqual([{ sessionID: "child-1", directory: RELOCATED }]);
    expect(worktreeForSession("parent-1")).toBeNull();
  });

  test("a parent's target without a proven origin never overrides the host", async () => {
    // No origin was proven for the parent's target, so it cannot justify
    // overriding the host's fresh directory: the child follows the host.
    rememberWorktree("parent-1", STALE);
    const moves = [];
    const sessions = {
      "parent-1": { parentID: null, location: { directory: RELOCATED } },
      "child-1": { parentID: "parent-1", location: { directory: MAIN } },
    };
    const context = hostContext(sessions, moves);
    const prompt = createPromptHook(context, noCliDeps);

    await expect(prompt(promptEvent("child-1"))).resolves.toBeUndefined();
    expect(moves).toEqual([{ sessionID: "child-1", directory: RELOCATED }]);
    expect(worktreeForSession("parent-1")).toBeNull();
  });

  test("an own target without a proven origin never overrides the host", async () => {
    // The same rule for the session's own target: with no proven origin it
    // cannot justify a move against the host's fresh directory.
    rememberWorktree("session-1", STALE);
    const moves = [];
    const sessions = {
      "session-1": { parentID: null, location: { directory: RELOCATED } },
    };
    const context = hostContext(sessions, moves);
    const prompt = createPromptHook(context, noCliDeps);

    await expect(prompt(promptEvent("session-1"))).resolves.toBeUndefined();
    expect(moves).toEqual([]);
    expect(worktreeForSession("session-1")).toBeNull();
  });

  test("a confirmed target moved back to its origin is an external relocation", async () => {
    // The parent was confirmed in the worktree and the host now reports its
    // origin again: after a confirmation even a move back to the origin is a
    // relocation, so the child follows the host.
    rememberWorktree("parent-1", STALE, MAIN);
    const moves = [];
    const sessions = {
      "parent-1": { parentID: null, location: { directory: STALE } },
      "child-1": { parentID: "parent-1", location: { directory: STALE } },
    };
    const context = hostContext(sessions, moves);
    const prompt = createPromptHook(context, noCliDeps);

    // The parent sits in its target: the placement is settled and confirmed.
    await expect(prompt(promptEvent("parent-1"))).resolves.toBeUndefined();
    expect(moves).toEqual([]);
    expect(sessionPlaced("parent-1")).toBe(true);

    // The parent is returned to its origin.
    sessions["parent-1"] = { parentID: null, location: { directory: MAIN } };
    await expect(prompt(promptEvent("child-1"))).resolves.toBeUndefined();
    expect(moves).toEqual([{ sessionID: "child-1", directory: MAIN }]);
    expect(worktreeForSession("parent-1")).toBeNull();
  });

  test("a stale parent session record never places the child", async () => {
    // The parent's cached session record names the old directory, but the host
    // reports where the parent actually is: the fresh answer wins.
    const moves = [];
    const sessions = {
      "parent-1": { parentID: null, location: { directory: STALE } },
      "child-1": { parentID: "parent-1", location: { directory: MAIN } },
    };
    const context = hostContext(sessions, moves);
    expect(await readSessionInfo(context, "parent-1")).toEqual({
      parentID: null,
      directory: STALE,
    });

    // The parent is relocated, and it has no registration of its own.
    sessions["parent-1"] = { parentID: null, location: { directory: RELOCATED } };
    const prompt = createPromptHook(context, noCliDeps);
    await expect(prompt(promptEvent("child-1"))).resolves.toBeUndefined();
    expect(moves).toEqual([{ sessionID: "child-1", directory: RELOCATED }]);
  });

  test("an unreadable host never falls back to a stale target", async () => {
    // The host cannot report where the session is, so the placement fails
    // closed instead of guessing a directory.
    rememberWorktree("session-1", STALE, MAIN);
    const moves = [];
    const context = {
      location: { directory: MAIN },
      session: {
        get: async () => {
          throw new Error("session store unavailable");
        },
        move: async (input) => moves.push(input),
      },
    };
    const prompt = createPromptHook(context, noCliDeps);
    await expect(prompt(promptEvent("session-1"))).rejects.toThrow(
      /session placement unverified/,
    );
    expect(moves).toEqual([]);
  });

  test("an equal directory needs no move", async () => {
    // The host reports the session in its registered target: the placement is
    // settled and confirmed, and nothing is moved.
    rememberWorktree("session-1", STALE, MAIN);
    const moves = [];
    const sessions = {
      "session-1": { parentID: null, location: { directory: STALE } },
    };
    const context = hostContext(sessions, moves);
    const prompt = createPromptHook(context, noCliDeps);

    await expect(prompt(promptEvent("session-1"))).resolves.toBeUndefined();
    expect(moves).toEqual([]);
    expect(sessionPlaced("session-1")).toBe(true);
    expect(worktreeForSession("session-1")).toBe(STALE);
  });

  test("a true first placement still happens from the recorded origin", async () => {
    // The session sits in the origin its target was decided from: the
    // placement is still pending, so the move happens as before.
    rememberWorktree("session-1", STALE, MAIN);
    const moves = [];
    const sessions = {
      "session-1": { parentID: null, location: { directory: MAIN } },
    };
    const context = hostContext(sessions, moves);
    const prompt = createPromptHook(context, noCliDeps);

    await expect(prompt(promptEvent("session-1"))).resolves.toBeUndefined();
    expect(moves).toEqual([{ sessionID: "session-1", directory: STALE }]);
    expect(sessionPlaced("session-1")).toBe(true);
  });

  test("an admitted pending move from a known origin keeps the target", async () => {
    // The parent's move was accepted but the host has not applied it yet: it
    // still reports the origin, so the child follows the pending target
    // instead of the lagging host answer.
    rememberWorktree("parent-1", STALE, MAIN);
    const moves = [];
    const sessions = {
      "parent-1": { parentID: null, location: { directory: MAIN } },
      "child-1": { parentID: "parent-1", location: { directory: MAIN } },
    };
    const context = hostContext(sessions, moves);

    await ensureSessionWorktree(context, "parent-1", noCliDeps);
    expect(moves).toEqual([{ sessionID: "parent-1", directory: STALE }]);

    // The host has not applied the accepted move yet.
    sessions["parent-1"] = { parentID: null, location: { directory: MAIN } };
    const prompt = createPromptHook(context, noCliDeps);
    await expect(prompt(promptEvent("child-1"))).resolves.toBeUndefined();
    expect(moves).toEqual([
      { sessionID: "parent-1", directory: STALE },
      { sessionID: "child-1", directory: STALE },
    ]);
  });

  test("consecutive own prompts do not rediscover the externally abandoned target", async () => {
    rememberWorktree("session-1", STALE, MAIN);
    const moves = [];
    const context = hostContext({
      "session-1": { parentID: null, location: { directory: RELOCATED } },
    }, moves);
    const deps = {
      ...noCliDeps,
      discover: async (sessionId) => {
        rememberWorktree(sessionId, STALE);
        return STALE;
      },
    };
    await ensureSessionWorktree(context, "session-1", deps);
    expect(moves).toEqual([]);
    await ensureSessionWorktree(context, "session-1", deps);
    expect(moves).toEqual([]);
  });

  test("consecutive child prompts do not re-inherit its externally abandoned target", async () => {
    rememberWorktree("child-1", STALE, MAIN);
    const moves = [];
    const context = hostContext({
      "parent-1": { parentID: null, location: { directory: STALE } },
      "child-1": { parentID: "parent-1", location: { directory: RELOCATED } },
    }, moves);
    const prompt = createPromptHook(context, noCliDeps);
    await prompt(promptEvent("child-1"));
    expect(moves).toEqual([]);
    await prompt(promptEvent("child-1"));
    expect(moves).toEqual([]);
  });

  test("a known-origin accepted pending target survives its own next prompt", async () => {
    rememberWorktree("parent-1", STALE, MAIN);
    const moves = [];
    const context = {
      location: { directory: MAIN },
      session: {
        get: async ({ sessionID }) => ({
          parentID: sessionID === "child-1" ? "parent-1" : null,
          location: { directory: MAIN },
        }),
        move: async (input) => moves.push(input),
      },
    };
    await ensureSessionWorktree(context, "parent-1", noCliDeps);
    expect(sessionPlaced("parent-1")).toBe(true);
    await ensureSessionWorktree(context, "parent-1", noCliDeps);
    await ensureSessionWorktree(context, "child-1", noCliDeps);
    expect(moves).toEqual([
      { sessionID: "parent-1", directory: STALE },
      { sessionID: "child-1", directory: STALE },
    ]);
    expect(worktreeForSession("parent-1")).toBe(STALE);
  });

  test("repeated prompts of a child of a relocated parent produce zero stale moves", async () => {
    rememberWorktree("parent-1", STALE, MAIN);
    const moves = [];
    const sessions = {
      "parent-1": { parentID: null, location: { directory: RELOCATED } },
      "child-1": { parentID: "parent-1", location: { directory: RELOCATED } },
    };
    const context = hostContext(sessions, moves);
    const prompt = createPromptHook(context, noCliDeps);

    for (let round = 0; round < 3; round += 1) {
      await expect(prompt(promptEvent("child-1"))).resolves.toBeUndefined();
    }
    // The child already sits where the parent is: no stale target is applied
    // on any of the prompts.
    expect(moves).toEqual([]);
    expect(worktreeForSession("parent-1")).toBeNull();
    // The child records the inherited target it already occupies, so a later
    // external relocation of the child can be reconciled.
    expect(worktreeForSession("child-1")).toBe(RELOCATED);
  });

  test("the dropped target takes its origin with it", async () => {
    // The origin belongs to the target it justified, so it cannot survive the
    // target and make a later registration look like a pending move.
    rememberWorktree("session-1", STALE, MAIN);
    const moves = [];
    const sessions = {
      "session-1": { parentID: null, location: { directory: RELOCATED } },
    };
    const context = hostContext(sessions, moves);

    expect(await reconcileSessionLocation(context, "session-1")).toBe(false);
    expect(worktreeForSession("session-1")).toBeNull();

    // A fresh registration for the same session starts from its own origin.
    rememberWorktree("session-1", STALE, RELOCATED);
    expect(await reconcileSessionLocation(context, "session-1")).toBe(true);
    expect(worktreeForSession("session-1")).toBe(STALE);
    expect(moves).toEqual([]);
  });

  test("a close before the placement is confirmed leaves the target pending", async () => {
    // The accepted move was never confirmed and the session is back at its
    // origin: that is indistinguishable from a move that has not landed yet,
    // so the target stays pending and the session is not moved back into it.
    rememberWorktree("session-1", STALE, MAIN);
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
    await expect(prompt(promptEvent("session-1"))).resolves.toBeUndefined();
    expect(moves).toEqual([STALE]);

    // The session is back at its origin before the placement was confirmed.
    directory = MAIN;
    await expect(prompt(promptEvent("session-1"))).resolves.toBeUndefined();
    expect(moves).toEqual([STALE]);
    expect(worktreeForSession("session-1")).toBe(STALE);
  });

  test("an abandoned target does not block a different one", async () => {
    // The suppression is target-specific: another worktree, a new issue, is
    // still placed.
    rememberWorktree("session-1", STALE, MAIN);
    const moves = [];
    const context = hostContext({
      "session-1": { parentID: null, location: { directory: RELOCATED } },
    }, moves);
    const other = "/repo/.worktrees/issue-other-directory";
    const deps = {
      ...noCliDeps,
      discover: async (sessionId) => {
        rememberWorktree(sessionId, other);
        return other;
      },
    };
    await ensureSessionWorktree(context, "session-1", deps);
    expect(moves).toEqual([]);

    // The next prompt rediscovers the same, different target and places it.
    await ensureSessionWorktree(context, "session-1", deps);
    expect(moves).toEqual([{ sessionID: "session-1", directory: other }]);
  });

  test("a parent's relocated target is abandoned for the parent itself", async () => {
    rememberWorktree("parent-1", STALE, MAIN);
    const moves = [];
    const sessions = {
      "parent-1": { parentID: null, location: { directory: RELOCATED } },
      "child-1": { parentID: "parent-1", location: { directory: MAIN } },
    };
    const context = hostContext(sessions, moves);
    const deps = {
      ...noCliDeps,
      discover: async (sessionId) => {
        rememberWorktree(sessionId, STALE);
        return STALE;
      },
    };
    const prompt = createPromptHook(context, deps);
    await expect(prompt(promptEvent("child-1"))).resolves.toBeUndefined();
    expect(moves).toEqual([{ sessionID: "child-1", directory: RELOCATED }]);

    // The parent's own rediscovery cannot resurrect the target it was moved
    // out of either.
    await expect(prompt(promptEvent("parent-1"))).resolves.toBeUndefined();
    expect(moves).toEqual([{ sessionID: "child-1", directory: RELOCATED }]);
  });

  test("a reset clears the abandoned target", async () => {
    rememberWorktree("session-1", STALE, MAIN);
    const moves = [];
    const context = hostContext({
      "session-1": { parentID: null, location: { directory: RELOCATED } },
    }, moves);
    const deps = {
      ...noCliDeps,
      discover: async (sessionId) => {
        rememberWorktree(sessionId, STALE);
        return STALE;
      },
    };
    await ensureSessionWorktree(context, "session-1", deps);
    expect(moves).toEqual([]);
    // The abandoned target is still refused on the next admission.
    await ensureSessionWorktree(context, "session-1", deps);
    expect(moves).toEqual([]);

    // A reset clears the record, so the target may be placed again.
    resetWorktrees();
    await ensureSessionWorktree(context, "session-1", deps);
    expect(moves).toEqual([{ sessionID: "session-1", directory: STALE }]);
  });

  test("the discovery probes run in the session's own directory", async () => {
    // The plugin's context location is not the session's directory, so the
    // probes follow the host.
    const probes = [];
    const moves = [];
    const context = {
      location: { directory: "/plugin/context" },
      session: {
        get: async () => ({ parentID: null, location: { directory: RELOCATED } }),
        move: async (input) => moves.push(input),
      },
    };
    const deps = {
      ...noCliDeps,
      discover: async (sessionId, cwd) => {
        probes.push(cwd);
        return null;
      },
    };
    await ensureSessionWorktree(context, "session-1", deps);
    expect(probes).toEqual([RELOCATED]);
  });

  test("a co-located child follows a different fresh parent target without reset", async () => {
    const moves = [];
    const sessions = {
      "parent-1": { parentID: null, location: { directory: STALE } },
      "child-1": { parentID: "parent-1", location: { directory: STALE } },
    };
    const prompt = createPromptHook(hostContext(sessions, moves), noCliDeps);
    await prompt(promptEvent("child-1"));
    expect(moves).toEqual([]);
    sessions["parent-1"] = { parentID: null, location: { directory: RELOCATED } };
    await prompt(promptEvent("child-1"));
    expect(moves).toEqual([{ sessionID: "child-1", directory: RELOCATED }]);
  });

  test("repeated child admissions preserve the known origin of an accepted pending move", async () => {
    const moves = [];
    const context = {
      location: { directory: MAIN },
      session: {
        get: async ({ sessionID }) => ({
          parentID: sessionID === "child-1" ? "parent-1" : null,
          location: { directory: sessionID === "child-1" ? MAIN : STALE },
        }),
        move: async (input) => moves.push(input),
      },
    };
    const prompt = createPromptHook(context, noCliDeps);
    for (let round = 0; round < 3; round += 1) {
      await prompt(promptEvent("child-1"));
    }
    expect(moves).toEqual([{ sessionID: "child-1", directory: STALE }]);
    expect(worktreeForSession("child-1")).toBe(STALE);
  });

  test("a superseded in-flight placement cannot move back to the old target", async () => {
    const { moveSessionToWorktree } = await import("./src/session.js");
    let releaseOldRead;
    let signalOldRead;
    const oldReadStarted = new Promise((resolve) => { signalOldRead = resolve; });
    const oldRead = new Promise((resolve) => { releaseOldRead = resolve; });
    const moves = [];
    let reads = 0;
    const context = {
      session: {
        get: async () => {
          if (++reads === 1) {
            signalOldRead();
            return await oldRead;
          }
          return { parentID: null, location: { directory: MAIN } };
        },
        move: async (input) => moves.push(input),
      },
    };
    rememberWorktree("child-1", STALE, MAIN);
    const oldAttempt = moveSessionToWorktree(context, "child-1", STALE);
    await oldReadStarted;
    rememberWorktree("child-1", RELOCATED, MAIN);
    await moveSessionToWorktree(context, "child-1", RELOCATED);
    releaseOldRead({ parentID: null, location: { directory: MAIN } });
    await oldAttempt;
    expect(worktreeForSession("child-1")).toBe(RELOCATED);
    expect(moves).toEqual([{ sessionID: "child-1", directory: RELOCATED }]);
  });

  test("a new target waits for the in-flight host move of the old target", async () => {
    const { moveSessionToWorktree } = await import("./src/session.js");
    let releaseOldMove;
    let signalOldMove;
    const oldMoveStarted = new Promise((resolve) => { signalOldMove = resolve; });
    const oldMove = new Promise((resolve) => { releaseOldMove = resolve; });
    const moves = [];
    const context = {
      session: {
        get: async () => ({ parentID: null, location: { directory: MAIN } }),
        move: async (input) => {
          moves.push(input);
          if (input.directory === STALE) {
            signalOldMove();
            await oldMove;
          }
        },
      },
    };
    rememberWorktree("child-1", STALE, MAIN);
    const oldAttempt = moveSessionToWorktree(context, "child-1", STALE);
    await oldMoveStarted;
    rememberWorktree("child-1", RELOCATED, MAIN);
    const newAttempt = moveSessionToWorktree(context, "child-1", RELOCATED);

    // The new request is not dispatched while the earlier host move is still
    // in flight, so it cannot be overtaken by its completion.
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(moves).toEqual([{ sessionID: "child-1", directory: STALE }]);

    releaseOldMove();
    await Promise.all([oldAttempt, newAttempt]);
    expect(moves).toEqual([
      { sessionID: "child-1", directory: STALE },
      { sessionID: "child-1", directory: RELOCATED },
    ]);
    expect(worktreeForSession("child-1")).toBe(RELOCATED);
  });

  test("equal-target suppression waits for an earlier dispatched move to finish", async () => {
    const { moveSessionToWorktree } = await import("./src/session.js");
    let releaseOldMove;
    let signalOldMove;
    const oldMoveStarted = new Promise((resolve) => { signalOldMove = resolve; });
    const oldMove = new Promise((resolve) => { releaseOldMove = resolve; });
    let directory = MAIN;
    const context = {
      session: {
        get: async () => ({ parentID: null, location: { directory } }),
        move: async (input) => {
          if (input.directory === STALE) {
            signalOldMove();
            await oldMove;
          }
          directory = input.directory;
        },
      },
    };
    rememberWorktree("child-1", STALE, MAIN);
    const oldAttempt = moveSessionToWorktree(context, "child-1", STALE);
    await oldMoveStarted;
    rememberWorktree("child-1", MAIN);
    const newAttempt = moveSessionToWorktree(context, "child-1", MAIN);
    releaseOldMove();
    await Promise.all([oldAttempt, newAttempt]);
    expect(worktreeForSession("child-1")).toBe(MAIN);
    expect(directory).toBe(MAIN);
  });

  test("the final host location follows the serialized request order", async () => {
    const { moveSessionToWorktree } = await import("./src/session.js");
    let releaseOldMove;
    let signalOldMove;
    const oldMoveStarted = new Promise((resolve) => { signalOldMove = resolve; });
    const oldMove = new Promise((resolve) => { releaseOldMove = resolve; });
    const moves = [];
    let directory = MAIN;
    const context = {
      session: {
        get: async () => ({ parentID: null, location: { directory } }),
        // The host applies the move it is given, in the order it was asked for.
        move: async (input) => {
          if (input.directory === STALE) {
            signalOldMove();
            await oldMove;
          }
          moves.push(input);
          directory = input.directory;
        },
      },
    };
    rememberWorktree("child-1", STALE, MAIN);
    const oldAttempt = moveSessionToWorktree(context, "child-1", STALE);
    await oldMoveStarted;
    rememberWorktree("child-1", RELOCATED, MAIN);
    const newAttempt = moveSessionToWorktree(context, "child-1", RELOCATED);
    releaseOldMove();
    await Promise.all([oldAttempt, newAttempt]);

    // The requests keep their order, and the session ends in the newer target.
    expect(moves.map((move) => move.directory)).toEqual([STALE, RELOCATED]);
    expect(directory).toBe(RELOCATED);
    expect(worktreeForSession("child-1")).toBe(RELOCATED);
  });

  test("a superseded attempt's rejection leaves the newer attempt intact", async () => {
    const { moveSessionToWorktree } = await import("./src/session.js");
    let releaseOldMove;
    let signalOldMove;
    const oldMoveStarted = new Promise((resolve) => { signalOldMove = resolve; });
    const oldMove = new Promise((resolve, reject) => {
      releaseOldMove = () => reject(new Error("runner not ready"));
    });
    const moves = [];
    const context = {
      session: {
        get: async () => ({ parentID: null, location: { directory: MAIN } }),
        move: async (input) => {
          moves.push(input);
          if (input.directory === STALE) {
            signalOldMove();
            await oldMove;
          }
        },
      },
    };
    rememberWorktree("child-1", STALE, MAIN);
    const oldAttempt = moveSessionToWorktree(context, "child-1", STALE);
    await oldMoveStarted;
    rememberWorktree("child-1", RELOCATED, MAIN);
    const newAttempt = moveSessionToWorktree(context, "child-1", RELOCATED);

    // The superseded attempt fails: its own cleanup must not retire the newer
    // attempt, and its failure must not fail the newer placement.
    releaseOldMove();
    await expect(oldAttempt).rejects.toThrow(/session placement failed/);
    await newAttempt;
    expect(moves).toEqual([
      { sessionID: "child-1", directory: STALE },
      { sessionID: "child-1", directory: RELOCATED },
    ]);
    expect(worktreeForSession("child-1")).toBe(RELOCATED);
  });

  test("the registration state follows the target lifecycle matrix", async () => {
    // The origin and the placement markers belong to one target: the same
    // target keeps them, a different target starts without them.
    const OTHER = "/repo/.worktrees/issue-other-target";

    // Brings a session into one placement state: never moved, a move that was
    // accepted but never confirmed, or a placement the host confirmed.
    async function establish(state) {
      resetWorktrees();
      rememberWorktree("session-1", STALE, MAIN);
      const moves = [];
      let directory = state === "confirmed" ? STALE : MAIN;
      const context = {
        location: { directory: MAIN },
        session: {
          get: async () => ({ parentID: null, location: { directory } }),
          // The host accepts the move without applying it, so the placement
          // stays unconfirmed.
          move: async (input) => moves.push(input),
        },
      };
      if (state !== "unplaced") {
        await createPromptHook(context, noCliDeps)(promptEvent("session-1"));
        moves.length = 0;
      }
      return { context, moves, setDirectory: (value) => { directory = value; } };
    }

    const cases = [
      // Same target: the host's answer decides whether it survives.
      { state: "unplaced", host: MAIN, kept: true, moved: true },
      { state: "unplaced", host: STALE, kept: true, moved: false },
      { state: "unplaced", host: RELOCATED, kept: false, moved: false },
      { state: "accepted", host: MAIN, kept: true, moved: false },
      { state: "accepted", host: STALE, kept: true, moved: false },
      { state: "accepted", host: RELOCATED, kept: false, moved: false },
      { state: "confirmed", host: MAIN, kept: false, moved: false },
      { state: "confirmed", host: STALE, kept: true, moved: false },
      { state: "confirmed", host: RELOCATED, kept: false, moved: false },
    ];
    for (const { state, host, kept, moved } of cases) {
      const { context, moves, setDirectory } = await establish(state);
      setDirectory(host);
      await createPromptHook(context, noCliDeps)(promptEvent("session-1"));
      expect({ state, host, target: worktreeForSession("session-1") }).toEqual({
        state,
        host,
        target: kept ? STALE : null,
      });
      expect({ state, host, moves: moves.length }).toEqual({
        state,
        host,
        moves: moved ? 1 : 0,
      });
    }

    // A different target invalidates the previous state: the old placed flag
    // never suppresses the new move.
    for (const state of ["unplaced", "accepted", "confirmed"]) {
      const { context, moves, setDirectory } = await establish(state);
      setDirectory(MAIN);
      rememberWorktree("session-1", OTHER, MAIN);
      await createPromptHook(context, noCliDeps)(promptEvent("session-1"));
      expect({ state, target: worktreeForSession("session-1") }).toEqual({
        state,
        target: OTHER,
      });
      expect({ state, moves: moves.length }).toEqual({ state, moves: 1 });
    }
  });

  test("an initially co-located child records the inherited target without a move", async () => {
    // The child already sits in the parent's directory: the placement is
    // recorded, so a later external relocation of the child can be reconciled,
    // and the equal directory needs no move.
    const moves = [];
    const sessions = {
      "parent-1": { parentID: null, location: { directory: STALE } },
      "child-1": { parentID: "parent-1", location: { directory: STALE } },
    };
    const context = hostContext(sessions, moves);
    const prompt = createPromptHook(context, noCliDeps);

    await expect(prompt(promptEvent("child-1"))).resolves.toBeUndefined();
    expect(moves).toEqual([]);
    expect(sessionPlaced("child-1")).toBe(true);
    expect(worktreeForSession("child-1")).toBe(STALE);

    // The next prompt keeps the same placement and issues no move either.
    await expect(prompt(promptEvent("child-1"))).resolves.toBeUndefined();
    expect(moves).toEqual([]);
    expect(worktreeForSession("child-1")).toBe(STALE);
  });

  test("a child-only external move is reconciled and never moved back", async () => {
    // The parent stays in the old target while the host moved the child out of
    // it: the child's own target is dropped and abandoned, and no later prompt
    // puts the child back into it.
    rememberWorktree("child-1", STALE, MAIN);
    const moves = [];
    const sessions = {
      "parent-1": { parentID: null, location: { directory: STALE } },
      "child-1": { parentID: "parent-1", location: { directory: RELOCATED } },
    };
    const context = hostContext(sessions, moves);
    const prompt = createPromptHook(context, noCliDeps);

    for (let round = 0; round < 3; round += 1) {
      await expect(prompt(promptEvent("child-1"))).resolves.toBeUndefined();
    }
    expect(moves).toEqual([]);
    expect(worktreeForSession("child-1")).toBeNull();
  });

  test("a different inherited target stays eligible after an abandonment", async () => {
    // The child abandoned one target; the parent moving to another directory
    // is still eligible for the child.
    rememberWorktree("child-1", STALE, MAIN);
    const moves = [];
    const sessions = {
      "parent-1": { parentID: null, location: { directory: STALE } },
      "child-1": { parentID: "parent-1", location: { directory: RELOCATED } },
    };
    const context = hostContext(sessions, moves);
    const prompt = createPromptHook(context, noCliDeps);

    // The child's own target is abandoned by the first prompt.
    await expect(prompt(promptEvent("child-1"))).resolves.toBeUndefined();
    expect(moves).toEqual([]);

    // The parent moved to a third directory: the child follows it.
    const third = "/repo/.worktrees/issue-third";
    sessions["parent-1"] = { parentID: null, location: { directory: third } };
    await expect(prompt(promptEvent("child-1"))).resolves.toBeUndefined();
    expect(moves).toEqual([{ sessionID: "child-1", directory: third }]);
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
      "phasegent issue create --title t --body b --session session-1 | tee /tmp/x",
    );
  });

  test("keeps an existing role assignment and leaves issue bind untouched", () => {
    expect(
      rewritePhasegentCommand(
        "phasegent --provider local issue bind 18",
        "abc",
        { agent: "orchestrator" },
      ),
    ).toBe("phasegent --provider local issue bind 18");
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
    ).toBe("phasegent issue status");
    expect(
      rewritePhasegentCommand("phasegent worktree status --issue 1", undefined, {
        agent: "reviewer",
      }),
    ).toBe("phasegent worktree status --issue 1");
  });

  test("leaves an existing role assignment untouched", () => {
    expect(
      rewritePhasegentCommand("phasegent issue get 1", "s1", {
        agent: "orchestrator",
      }),
    ).toBe("phasegent issue get 1");
    expect(
      rewritePhasegentCommand("phasegent issue get 1", "s1", {
        agent: "orchestrator",
      }),
    ).toBe("phasegent issue get 1");
    expect(
      rewritePhasegentCommand("phasegent issue get 1", "s1", {
        agent: "executor",
      }),
    ).toBe("phasegent issue get 1");
  });

  test("never writes a role into the command text on any host", () => {
    // Issue #22 P1: the role travels out of band through the host broker,
    // so no shell (POSIX or PowerShell) ever sees a role assignment.
    for (const windows of [true, false]) {
      expect(
        rewritePhasegentCommand(
          "phasegent issue status",
          "s1",
          { agent: "executor" },
          { windows },
        ),
      ).toBe("phasegent issue status");
      expect(
        rewritePhasegentCommand(
          "phasegent issue get 1; Write-Output $env:PHASEGENT_ROLE",
          "s1",
          { agent: "executor" },
          { windows },
        ),
      ).toBe("phasegent issue get 1; Write-Output $env:PHASEGENT_ROLE");
      expect(
        rewritePhasegentCommand(
          "phasegent issue status && phasegent issue get 1",
          "s1",
          { agent: "executor" },
          { windows },
        ),
      ).toBe("phasegent issue status && phasegent issue get 1");
    }
  });

  test("leaves a removed --role flag for the CLI to reject", () => {
    // The flag is intentionally incompatible (issue #588): the adapter adds the
    // role assignment and never rewrites the flag itself.
    expect(
      rewritePhasegentCommand("phasegent --role orchestrator issue status", "s1", {
        agent: "executor",
      }),
    ).toBe("phasegent --role orchestrator issue status");
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
      "phasegent issue get 1 && phasegent issue bind 541",
    );
  });

  test("rewrites an invocation behind env assignments or a path prefix", () => {
    expect(
      rewritePhasegentCommand(
        "PHASEGENT_WORKTREE_NO_DISCOVER=1 phasegent issue status",
        "s1",
        { agent: "executor" },
      ),
    ).toBe("PHASEGENT_WORKTREE_NO_DISCOVER=1 phasegent issue status");
    expect(
      rewritePhasegentCommand("./phasegent issue status", "s1", { agent: "executor" }),
    ).toBe("./phasegent issue status");
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
      "phasegent issue create --title t",
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
      rewritePhasegentCommand("phasegent issue close 1", "s1", {
        agent: "executor",
      }),
    ).toBe("phasegent issue close 1 --worktree-session s1");
    expect(
      rewritePhasegentCommand("phasegent issue status", "s1", {
        agent: "reviewer",
      }),
    ).toBe("phasegent issue status");
  });

  test("refuses a visible role assignment before a sub-agent invocation", () => {
    // Issue #22 P3: an inline managed-variable mutation is refused whole, not
    // outranked, because it could also replace the broker transport.
    const out = rewritePhasegentCommand(
      "PHASEGENT_ROLE='orchestrator' phasegent issue status",
      "s1",
      { agent: "executor" },
    );
    expect(out).toContain("refusing this command");
    expect(out).not.toContain("PHASEGENT_ROLE='orchestrator'");
    expect(out).not.toContain("issue status");
  });

  test("outranks a differing role claim for a sub-agent", () => {
    expect(
      rewritePhasegentCommand(
        "phasegent issue status",
        "s1",
        { agent: "executor" },
      ),
    ).toBe("phasegent issue status");
  });

  test("keeps an orchestrator session's own role claim", () => {
    // `issue close` additionally gets its closer flag (issue #575 P2).
    expect(
      rewritePhasegentCommand("phasegent issue close 1", "s1", {
        agent: "orchestrator",
      }),
    ).toBe("phasegent issue close 1 --worktree-session s1");
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
      "phasegent record create 754 --kind recon --key scan-1 --recon repo --authorized --body t",
    );
    expect(
      rewritePhasegentCommand(
        "phasegent record create 754 --kind reviewer --key r-1 --authorized --body t",
        "s1",
        { agent: "explore" },
      ),
    ).toBe(
      "phasegent record create 754 --kind reviewer --key r-1 --authorized --body t",
    );
    expect(
      rewritePhasegentCommand(
        "phasegent record create 754 --kind executor --key e-1 --phase P1 --attempt 1 --body t",
        "s1",
        { agent: "executor" },
      ),
    ).toBe(
      "phasegent record create 754 --kind executor --key e-1 --phase P1 --attempt 1 --body t",
    );
    // A record read is still a phasegent invocation, so it gets the role too.
    expect(
      rewritePhasegentCommand("phasegent record get 754 42", "s1", { agent: "explore" }),
    ).toBe("phasegent record get 754 42");
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
describe("managed environment mutation guard (issue #22 P3)", () => {
  const refusalFor = (command) =>
    rewritePhasegentCommand(command, "s1", { agent: "executor" });
  const refused = [
    "PHASEGENT_BROKER_ADDR=127.0.0.1:9 phasegent issue status",
    "PHASEGENT_BROKER_TOKEN=fake-token phasegent issue status",
    "PHASEGENT_SESSION_ID=other phasegent issue status",
    "OPENCODE_SESSION_ID=other phasegent issue status",
    "PHASEGENT_ROLE=orchestrator phasegent issue status",
    "PHASEGENT_SESSION_ID+=more phasegent issue status",
    'unset "PHASEGENT_SESSION_ID"; phasegent issue status',
    "unset -v OPENCODE_SESSION_ID; phasegent issue status",
    "export PHASEGENT_ROLE=orchestrator; phasegent issue status",
    "export PHASEGENT_BROKER_TOKEN; phasegent issue status",
    "declare -x PHASEGENT_SESSION_ID=other; phasegent issue status",
    "env PHASEGENT_BROKER_ADDR=127.0.0.1:9 phasegent issue status",
    'env PHASEGENT_SESSION_ID="a b" phasegent issue status',
    "env -u PHASEGENT_SESSION_ID phasegent issue status",
    "env --unset=PHASEGENT_BROKER_TOKEN phasegent issue status",
    "env -i phasegent issue status",
    "timeout 5s env -u PHASEGENT_BROKER_ADDR phasegent issue status",
    "command env PHASEGENT_ROLE=orchestrator phasegent issue status",
    "if PHASEGENT_ROLE=orchestrator; then phasegent issue status; fi",
    "true && env -u PHASEGENT_BROKER_TOKEN phasegent issue status",
    "phasegent issue status | env -u PHASEGENT_SESSION_ID cat",
    "foo; unset PHASEGENT_SESSION_ID; phasegent issue status",
    "builtin unset PHASEGENT_SESSION_ID; phasegent issue status",
    "builtin export PHASEGENT_ROLE=orchestrator; phasegent issue status",
    "builtin declare PHASEGENT_SESSION_ID=other; phasegent issue status",
    "> /dev/null unset PHASEGENT_SESSION_ID; phasegent issue status",
    "2>&1 unset PHASEGENT_BROKER_TOKEN; phasegent issue status",
    "read PHASEGENT_SESSION_ID <<< x; phasegent issue status",
    "read --array PHASEGENT_ROLE <<< x; phasegent issue status",
    "printf -v PHASEGENT_SESSION_ID x; phasegent issue status",
    "mapfile -t PHASEGENT_ROLE <<< x; phasegent issue status",
    "getopts a PHASEGENT_SESSION_ID; phasegent issue status",
  ];

  test("refuses every visible managed-variable mutation shape", () => {
    for (const command of refused) {
      const out = refusalFor(command);
      expect(out, command).toContain("refusing this command");
      expect(out, command).not.toContain("issue status");
      expect(out, command).not.toContain("127.0.0.1:9");
      expect(out, command).not.toContain("orchestrator");
      expect(out, command).not.toContain("fake-token");
    }
  });

  test("leaves ordinary arguments, look-alikes, and heredoc data untouched", () => {
    const allowed = [
      'phasegent issue get 1 --body "PHASEGENT_ROLE=orchestrator"',
      'echo "PHASEGENT_ROLE=x"; phasegent issue status',
      "env | grep PHASEGENT_ROLE; phasegent issue status",
      'FOO="PHASEGENT_ROLE=admin" phasegent issue get 1',
      "grep -n PHASEGENT_BROKER_ADDR= docs/host-verification.md; phasegent issue status",
      'printf "%s\\n" PHASEGENT_ROLE; phasegent issue status',
      'read -p "PHASEGENT_ROLE is not identity" NAME <<< value; phasegent issue status',
      "builtin echo PHASEGENT_ROLE=x; phasegent issue status",
      "> /dev/null echo PHASEGENT_SESSION_ID=x; phasegent issue status",
    ];
    for (const command of allowed) {
      const out = refusalFor(command);
      expect(out, command).not.toContain("refusing this command");
      expect(out, command).toContain("phasegent");
    }
    const heredoc = "cat <<'EOF'\nunset PHASEGENT_SESSION_ID\nEOF\nphasegent issue status";
    const out = refusalFor(heredoc);
    expect(out).not.toContain("refusing this command");
    expect(out).toContain("phasegent issue status");
  });

  test("refuses an unterminated quote instead of guessing shell syntax", () => {
    const out = refusalFor("phasegent issue status 'unterminated");
    expect(out).toContain("refusing this command");
  });

  test("the hook funnel returns the refusal before execution", async () => {
    const hook = createRedirectHook();
    const event = {
      tool: "bash",
      sessionID: "s1",
      agent: "executor",
      input: {
        command:
          "env -u PHASEGENT_SESSION_ID PHASEGENT_BROKER_ADDR=127.0.0.1:9 phasegent issue status",
      },
    };
    await hook(event);
    expect(event.input.command).toContain("refusing this command");
    expect(event.input.command).not.toContain("PHASEGENT_BROKER_ADDR=");
    expect(event.input.command).not.toContain("issue status");
  });
});

describe("POSIX transparent wrappers (issue #917 P1)", () => {
  test("leaves a wrapped invocation byte-for-byte", () => {
    // Issue #22 P1: no role is injected any more, so the wrapper needs no
    // `env` scope and the command keeps its exact bytes.
    expect(
      rewritePhasegentCommand("timeout 40s phasegent issue get 1", "s1", { agent: "executor" }),
    ).toBe("timeout 40s phasegent issue get 1");
  });

  test("keeps a user assignment before the wrapper and the session flag after", () => {
    expect(
      rewritePhasegentCommand(
        "TMPDIR=/tmp timeout 40s phasegent issue create --title t",
        "s1",
        { agent: "orchestrator" },
      ),
    ).toBe(
      "TMPDIR=/tmp timeout 40s phasegent issue create --title t --session s1",
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
      "timeout --signal=KILL -k 1s --preserve-status 40s phasegent issue status",
    );
    expect(
      rewritePhasegentCommand("timeout -s TERM 40s -- phasegent issue status", "s1", {
        agent: "executor",
      }),
    ).toBe("timeout -s TERM 40s -- phasegent issue status");
  });

  test("handles env assignments and command wrappers nested with timeout", () => {
    expect(
      rewritePhasegentCommand("env FOO=1 phasegent issue status", "s1", { agent: "executor" }),
    ).toBe("env FOO=1 phasegent issue status");
    expect(
      rewritePhasegentCommand("command phasegent issue status", "s1", { agent: "executor" }),
    ).toBe("command phasegent issue status");
    expect(
      rewritePhasegentCommand("timeout 5s env FOO=1 command phasegent issue status", "s1", {
        agent: "reviewer",
      }),
    ).toBe("timeout 5s env FOO=1 command phasegent issue status");
  });

  test("is idempotent on a rewritten env prefix", () => {
    const once = rewritePhasegentCommand("timeout 40s phasegent issue get 1", "s1", {
      agent: "executor",
    });
    expect(rewritePhasegentCommand(once, "s1", { agent: "executor" })).toBe(once);
    const userAssignment = rewritePhasegentCommand(
      "timeout 40s phasegent issue get 1",
      "s1",
      { agent: "executor" },
    );
    expect(userAssignment).toBe("timeout 40s phasegent issue get 1");
  });

  test("leaves non-wrapper commands and lookups byte-for-byte", () => {
    for (const command of [
      "sudo phasegent issue status",
      "sh -c phasegent issue status",
      "xargs phasegent",
      "timeout 40s",
      "command -v phasegent",
    ]) {
      expect(rewritePhasegentCommand(command, "s1", { agent: "executor" })).toBe(command);
    }
    // Issue #22 P3: an ambiguous wrapper cannot be proven safe, so it is
    // refused instead of being left executable.
    const ambiguous = rewritePhasegentCommand(
      "timeout --bogus 40s phasegent issue status",
      "s1",
      { agent: "executor" },
    );
    expect(ambiguous).toContain("refusing this command");
  });

  test("downgrades a claimed role and refuses issue create behind a wrapper", () => {
    expect(
      rewritePhasegentCommand(
        "timeout 5s phasegent issue status",
        "s1",
        { agent: "executor" },
      ),
    ).toBe("timeout 5s phasegent issue status");
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

  test("keeps the invocation unchanged for every canonical role behind a wrapper", () => {
    for (const agent of ["orchestrator", "executor", "reviewer", "explore"]) {
      expect(rewritePhasegentCommand("timeout 5s phasegent issue get 1", "s1", { agent })).toBe(
        "timeout 5s phasegent issue get 1",
      );
    }
  });

  test("leaves quoted data and heredoc bodies untouched behind a wrapper", () => {
    expect(
      rewritePhasegentCommand('timeout 5s phasegent issue get 1 --body "a && b"', "s1", {
        agent: "executor",
      }),
    ).toBe('timeout 5s phasegent issue get 1 --body "a && b"');
    const heredoc =
      "timeout 5s phasegent issue get 1 <<'EOF'\n" +
      "timeout 5s phasegent issue status\n" +
      "EOF";
    expect(rewritePhasegentCommand(heredoc, "s1", { agent: "executor" })).toBe(
      "timeout 5s phasegent issue get 1 <<'EOF'\n" +
        "timeout 5s phasegent issue status\n" +
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

    runtimeTest("runs the wrapped command unchanged through each wrapper shape", () => {
      for (const command of [
        "timeout 40s phasegent issue get 1",
        "TMPDIR=/tmp timeout 40s phasegent issue get 1",
        "env FOO=1 phasegent issue get 1",
        "command phasegent issue get 1",
        "timeout 5s env FOO=1 phasegent issue get 1",
      ]) {
        const result = run(command);
        // Issue #22 P1: the wrapper injects no role, so the fake CLI sees no
        // role variable at all.
        expect(result.stdout).toContain("role=<absent>");
        expect(result.status).toBe(0);
      }
    });

    runtimeTest("propagates a wrapped CLI failure through && and ||", () => {
      const and = run("timeout 40s phasegent issue get 1 && echo done", { code: 3 });
      expect(and.stdout).toContain("role=<absent>");
      expect(and.stdout).not.toContain("done");
      const or = run("timeout 40s phasegent issue get 1 || echo fallback", { code: 3 });
      expect(or.stdout).toContain("fallback");
    });

    runtimeTest("leaves no role variable behind for the next statement", () => {
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
      "phasegent issue close 575 --worktree-session session-1",
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
      "phasegent issue close 575 --worktree-session s1 && " +
      "phasegent issue sync";
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
      'phasegent issue close 575 --worktree-session "s1 s2"',
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
      'phasegent issue create --title t --body "a | b" --session session-1',
    );
  });

  test("keeps --session outside a multiline --body", () => {
    // A real newline inside the value must not split the segment.
    const command = 'phasegent issue create --title t --body "l1\nl2"';
    expect(rewritePhasegentCommand(command, "session-1", { agent: "orchestrator" })).toBe(
      'phasegent issue create --title t --body "l1\nl2" --session session-1',
    );
  });

  test("keeps --session outside a quoted && in --title", () => {
    expect(
      rewritePhasegentCommand('phasegent issue create --title "a && b"', "session-1", {
        agent: "orchestrator",
      }),
    ).toBe(
      'phasegent issue create --title "a && b" --session session-1',
    );
  });

  test("refuses an unterminated quote segment instead of guessing shell syntax", () => {
    for (const command of [
      'phasegent issue create --body "a | b',
      "phasegent issue create --title 'a && b",
      'phasegent issue status --body "a  ',
    ]) {
      expect(
        rewritePhasegentCommand(command, "session-1", { agent: "orchestrator" }),
      ).toContain("refusing this command");
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
      'phasegent issue create --title t --body "--session s9" --session session-1',
    );
  });

  test("does not mistake a quoted issue bind for an issue write", () => {
    expect(
      rewritePhasegentCommand('phasegent issue get 1 --body "issue bind"', "session-1", {
        agent: "executor",
      }),
    ).toBe('phasegent issue get 1 --body "issue bind"');
  });

  test("does not rewrite a quoted --role value for a sub-agent", () => {
    expect(
      rewritePhasegentCommand('phasegent issue get 1 --body "--role orchestrator"', "s1", {
        agent: "executor",
      }),
    ).toBe('phasegent issue get 1 --body "--role orchestrator"');
  });

  test("does not rewrite a quoted PHASEGENT_ROLE value for a sub-agent", () => {
    expect(
      rewritePhasegentCommand(
        'phasegent issue get 1 --body "PHASEGENT_ROLE=orchestrator"',
        "s1",
        { agent: "executor" },
      ),
    ).toBe(
      'phasegent issue get 1 --body "PHASEGENT_ROLE=orchestrator"',
    );
  });

  test("does not read a quoted role literal before the invocation as an assignment", () => {
    expect(
      rewritePhasegentCommand(
        'FOO="PHASEGENT_ROLE=admin" phasegent issue get 1',
        "s1",
        { agent: "executor" },
      ),
    ).toBe('FOO="PHASEGENT_ROLE=admin" phasegent issue get 1');
  });

  test("handles single-quoted values and escaped quotes", () => {
    expect(
      rewritePhasegentCommand(
        "phasegent issue create --title t --body 'a | b'",
        "s1",
        { agent: "orchestrator" },
      ),
    ).toBe(
      "phasegent issue create --title t --body 'a | b' --session s1",
    );
    expect(
      rewritePhasegentCommand(
        'phasegent issue create --title t --body "a \\" | b"',
        "s1",
        { agent: "orchestrator" },
      ),
    ).toBe(
      'phasegent issue create --title t --body "a \\" | b" --session s1',
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
      "phasegent issue create --title t --body b --session session-1 | tee /tmp/x",
    );
    expect(
      rewritePhasegentCommand(
        'phasegent issue bind 541 --note "a && b" | tee /tmp/y',
        "s9",
        { agent: "orchestrator" },
      ),
    ).toBe(
      'phasegent issue bind 541 --note "a && b" | tee /tmp/y',
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
      'phasegent issue status; echo "a | b"; phasegent worktree status',
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
      'PHASEGENT_WORKTREE_NO_DISCOVER="1" phasegent issue status',
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
      "cd /tmp && cat > /tmp/body.txt <<'BODY'\nit's the body with apostrophe\nBODY\nphasegent record create 853 --kind recon --recon mixed --key k --authorized --body-file /tmp/body.txt",
    );
    expect(out).toContain("it's the body with apostrophe");
    // No role injected inside the body.
    expect(out.split("it's the body")[0]).not.toContain("phasegent record");
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
      expect(out.endsWith("phasegent issue get 1")).toBe(true);
      expect(out).toContain(`${decl}\n${body}\n${terminator}`);
    }
  });

  test("<<- tab stripping honors leading tabs but not spaces", () => {
    const tabbed = "cat <<-EOF\n\tbody\n\tEOF\nphasegent issue get 1";
    expect(rewritePhasegentCommand(tabbed, "s1", EXPLORE)).toBe(
      "cat <<-EOF\n\tbody\n\tEOF\nphasegent issue get 1",
    );
    // Leading spaces do not terminate <<-: conservatively preserved.
    const spaced = "cat <<-EOF\n   body\n   EOF\nphasegent issue get 1";
    expect(rewritePhasegentCommand(spaced, "s1", EXPLORE)).toBe(spaced);
  });

  test("multiple queued bodies in order stay opaque and trailing rewrites", () => {
    const command = "cat <<A\nbodyA\nA\ncat <<B\nbodyB\nB\nphasegent issue get 1";
    expect(rewritePhasegentCommand(command, "s1", EXPLORE)).toBe(
      "cat <<A\nbodyA\nA\ncat <<B\nbodyB\nB\nphasegent issue get 1",
    );
    const queued = "cat <<A && cat <<B\nbodyA\nA\nbodyB\nB\nphasegent issue get 1";
    const out = rewritePhasegentCommand(queued, "s1", EXPLORE);
    expect(out.endsWith("phasegent issue get 1")).toBe(true);
    expect(out).toContain("bodyA\nA\nbodyB\nB");
  });

  test("fake elevated invocations and role claims in bodies stay untouched", () => {
    const command =
      "cat <<'EOF'\nphasegent issue create --title t --body b\nphasegent issue get 9\nADMIN phasegent issue get 1\nEOF\nphasegent issue get 2";
    const out = rewritePhasegentCommand(command, "s1", EXPLORE);
    expect(out).toBe(
      "cat <<'EOF'\nphasegent issue create --title t --body b\nphasegent issue get 9\nADMIN phasegent issue get 1\nEOF\nphasegent issue get 2",
    );
    // The downgrade path also leaves body literals alone for subagents.
    const sub = rewritePhasegentCommand(command, "s1", { agent: "executor" });
    expect(sub).toContain("phasegent issue get 9");
    expect(sub.endsWith("phasegent issue get 2")).toBe(true);
  });

  test("here-strings and quoted operators are not heredocs", () => {
    expect(
      rewritePhasegentCommand('cat <<< "hello phasegent issue get 9"\nphasegent issue get 1', "s1", EXPLORE),
    ).toBe('cat <<< "hello phasegent issue get 9"\nphasegent issue get 1');
    expect(rewritePhasegentCommand('"<<"EOF""\nphasegent issue get 1', "s1", EXPLORE)).toContain(
      "phasegent issue get 1",
    );
    expect(rewritePhasegentCommand("# cat <<EOF\nphasegent issue get 1", "s1", EXPLORE)).toBe(
      "# cat <<EOF\nphasegent issue get 1",
    );
  });

  test("escaped delimiters and quoted declaration text stay code-safe", () => {
    expect(rewritePhasegentCommand("cat <<\\EOF\nbody\nEOF\nphasegent issue get 1", "s1", EXPLORE)).toBe(
      "cat <<\\EOF\nbody\nEOF\nphasegent issue get 1",
    );
    expect(rewritePhasegentCommand('echo "<<EOF"\nphasegent issue get 1', "s1", EXPLORE)).toBe(
      'echo "<<EOF"\nphasegent issue get 1',
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
      "phasegent issue get 0\ncat <<EOF\nbody\nEOF\nphasegent issue get 1",
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
      input: { command: "phasegent issue status", workdir: "/tmp" },
    };
    await hook(event);
    expect(event.input.command).toBe("phasegent issue status");
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

describe("verified registry directory (lazy discovery)", () => {
  function orchContext(moves) {
    return {
      location: { directory: "/repo" },
      session: {
        get: async () => ({ parentID: null, location: { directory: "/repo" } }),
        move: async (input) => moves.push(input),
      },
    };
  }

  function directory(overrides) {
    return {
      directory_id: "dir-1",
      repo_identity: "/repo/.git",
      checkout_path: "/repo",
      worktree_path: WORKTREE,
      branch: "feat/575",
      created_at: 1,
      tasks: [],
      legacy_issues: [{ issue: 575, provider: "", project: "", external_id: 0 }],
      sessions: [],
      ...overrides,
    };
  }

  test("picks the one directory associated with the issue", () => {
    expect(verifiedIssueDirectory([directory()], 575, "ses_a")).toBe(WORKTREE);
    expect(
      verifiedIssueDirectory([directory({ sessions: ["ses_a"] })], 575, "ses_a"),
    ).toBe(WORKTREE);
  });

  test("rejects ambiguous, foreign-session and unrelated directories", () => {
    expect(
      verifiedIssueDirectory(
        [directory(), directory({ directory_id: "dir-2", worktree_path: "/wt/other" })],
        575,
        "ses_a",
      ),
    ).toBeNull();
    expect(
      verifiedIssueDirectory([directory({ sessions: ["ses_other"] })], 575, "ses_a"),
    ).toBeNull();
    expect(verifiedIssueDirectory([directory()], 576, "ses_a")).toBeNull();
    expect(verifiedIssueDirectory(null, 575, "ses_a")).toBeNull();
  });

  test("requires the complete immutable task id and instance, not the number", () => {
    const binding = {
      issueId: 575,
      provider: "forgejo",
      project: "owner/repo",
      externalId: "575",
      taskId: 42,
      instance: "https://forgejo.test",
    };
    const task = {
      task_id: 42,
      issue: 575,
      provider: "forgejo",
      instance: "https://forgejo.test",
      project: "owner/repo",
      external_id: "575",
    };
    const scoped = directory({ tasks: [task], legacy_issues: [] });
    expect(verifiedBindingDirectory([scoped], binding, "ses_a")).toBe(WORKTREE);
    expect(
      verifiedBindingDirectory(
        [directory({ tasks: [{ ...task, task_id: 41 }], legacy_issues: [] })],
        binding,
        "ses_a",
      ),
    ).toBeNull();
    expect(
      verifiedBindingDirectory(
        [directory({ tasks: [{ ...task, instance: "https://other.test" }], legacy_issues: [] })],
        binding,
        "ses_a",
      ),
    ).toBeNull();
    expect(
      verifiedBindingDirectory(
        [directory({ tasks: [], legacy_issues: [{ issue: 575, provider: "forgejo", project: "owner/repo", external_id: 575 }] })],
        binding,
        "ses_a",
      ),
    ).toBeNull();
    expect(
      verifiedBindingDirectory([scoped], { ...binding, project: "other/repo" }, "ses_a"),
    ).toBeNull();
    expect(
      verifiedBindingDirectory([scoped], { ...binding, instance: "" }, "ses_a"),
    ).toBeNull();
  });

  test("keeps the session in the current checkout with isolation guidance", async () => {
    const moves = [];
    const warnings = [];
    const original = console.warn;
    console.warn = (message) => warnings.push(String(message));
    const deps = {
      readSessionInfo: async () => null,
      discover: async () => null,
      readBinding: async () => ({ issueId: 575, provider: "forgejo", project: "owner/repo", externalId: "575", taskId: 42, instance: "https://forgejo.test" }),
    };
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
      const deps = {
        readSessionInfo: async () => null,
        discover: async () => {
          reads += 1;
          return null;
        },
      };
      expect(await ensureSessionWorktree(orchContext([]), "orch-4", deps)).toBeNull();
      expect(reads).toBe(0);
    } finally {
      restoreNoDiscover(saved);
    }
  });
});

describe("complete worktree directory page readers", () => {
  function fakePageRunner(pages, failures = []) {
    const calls = [];
    return {
      calls,
      execute: async (args) => {
        calls.push([...args]);
        const number = args[args.indexOf("--page") + 1];
        const key = `${args[1]}-page-${number}`;
        if (failures.includes(key) || pages[key] === undefined) return { ok: false };
        return { ok: true, value: JSON.stringify(pages[key]) };
      },
    };
  }

  function directory(index, issue = 42) {
    return {
      directory_id: `dir-${index}`,
      repo_identity: "/repo/.git",
      checkout_path: "/repo",
      worktree_path: `/repo/wt-${index}`,
      branch: `phasegent/${index}`,
      created_at: index + 1,
      tasks: [],
      legacy_issues: [{ issue, provider: "", project: "", external_id: 0 }],
      sessions: [],
    };
  }

  function page(items, number, total, context, extra = {}) {
    return {
      items,
      page: number,
      limit: 100,
      total_count: total,
      has_more: number * 100 < total,
      ...context,
      ...extra,
    };
  }

  test("traverses every page of the issue and repository collections", async () => {
    const active = Array.from({ length: 205 }, (_, index) => directory(index));
    const history = Array.from({ length: 205 }, (_, index) =>
      directory(index, index >= 200 ? 42 : 99),
    );
    const pages = {};
    for (let number = 1; number <= 3; number += 1) {
      const start = (number - 1) * 100;
      pages[`status-page-${number}`] = page(
        active.slice(start, start + 100),
        number,
        active.length,
        { issue: 42 },
      );
      pages[`list-page-${number}`] = page(
        history.slice(start, start + 100),
        number,
        history.length,
        { repo_identity: "/repo/.git" },
      );
    }
    const fake = fakePageRunner(pages);
    const activeRows = await readIssueDirectories(42, "/repo", fake.execute);
    const historyRows = await readRepositoryDirectories("/repo", fake.execute);
    expect(activeRows).toHaveLength(205);
    expect(historyRows.map((row) => row.directory_id)).toEqual(
      history.map((row) => row.directory_id),
    );
    expect(fake.calls).toHaveLength(6);
    expect(fake.calls[0].join(" ")).toContain("worktree status --issue 42 --page 1 --limit 100");
    expect(fake.calls[2].join(" ")).toContain("--page 3 --limit 100");
    expect(fake.calls[3].join(" ")).toContain("worktree list --no-sync --page 1 --limit 100");
    expect(fake.calls[5].join(" ")).toContain("--page 3 --limit 100");
  });

  test("rejects a repository identity change during traversal", async () => {
    const first = Array.from({ length: 100 }, (_, index) => directory(index));
    const fake = fakePageRunner({
      "list-page-1": page(first, 1, 101, { repo_identity: "/repo/.git" }),
      "list-page-2": page([directory(100)], 2, 101, { repo_identity: "/other/.git" }),
    });
    expect(await readRepositoryDirectories("/repo", fake.execute)).toBeNull();
    expect(fake.calls).toHaveLength(2);
  });

  test("returns null rather than partial rows after a later page failure", async () => {
    const first = Array.from({ length: 100 }, (_, index) => directory(index));
    const fake = fakePageRunner(
      { "status-page-1": page(first, 1, 150, { issue: 42 }) },
      ["status-page-2"],
    );
    expect(await readIssueDirectories(42, "/repo", fake.execute)).toBeNull();
    expect(fake.calls).toHaveLength(2);
  });

  test("fails closed on unpaged, duplicate, changing or nonprogressing pages", async () => {
    const first = Array.from({ length: 100 }, (_, index) => directory(index));
    const next = directory(100);
    const cases = [
      { pages: { "status-page-1": { issue: 42, directories: first } } },
      {
        pages: {
          "status-page-1": page(first, 1, 101, { issue: 42 }),
          "status-page-2": page([next], 1, 101, { issue: 42 }),
        },
      },
      {
        pages: {
          "status-page-1": page(first, 1, 101, { issue: 42 }),
          "status-page-2": page([directory(0)], 2, 101, { issue: 42 }),
        },
      },
      {
        pages: {
          "status-page-1": page(first, 1, 101, { issue: 42 }),
          "status-page-2": page([next, directory(101)], 2, 102, { issue: 42 }),
        },
      },
      {
        pages: {
          "status-page-1": page(first, 1, 101, { issue: 42 }),
          "status-page-2": page([next], 2, 101, { issue: 43 }),
        },
      },
      { pages: { "status-page-1": page(first.slice(0, 99), 1, 101, { issue: 42 }) } },
      { pages: { "status-page-1": page(first, 1, 1_000_001, { issue: 42 }) } },
    ];
    for (const { pages } of cases) {
      const fake = fakePageRunner(pages);
      expect(await readIssueDirectories(42, "/repo", fake.execute)).toBeNull();
    }
  });
});

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
