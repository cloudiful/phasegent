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
  rememberWorktree,
  worktreeForSession,
  resetWorktrees,
  createRedirectHook,
  createPromptHook,
  pickActiveWorktreePath,
  issueClosedLocally,
  ensureSessionWorktree,
  readSessionInfo,
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
  MCP_SERVER_ROLE,
  PHASEGENT_MCP_SERVER,
  applyServer,
  forgetMcpRegistration,
  hasPhasegentServer,
  mcpRegistered,
  phasegentMcpServerDefinition,
  registerPhasegentMcp,
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
  test("remembers the active worktree for unknown sessions", () => {
    rememberWorktree("session-1", WORKTREE);
    expect(worktreeForSession("session-1")).toBe(WORKTREE);
    // Task-spawned sub-agents carry a different (or absent) session id.
    expect(worktreeForSession("session-2")).toBe(WORKTREE);
    expect(worktreeForSession(undefined)).toBe(WORKTREE);
  });

  test("resetWorktrees clears the fallback so sessions pass through", () => {
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

  // `discover`/`readBinding` are stubbed so a missed inheritance fails loudly
  // instead of spawning the phasegent CLI; `readSessionInfo` stays real and
  // reads the fake host. The lazy path never creates a worktree on its own
  // (issue 616), so there is no acquire seam to stub.
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

  test("keeps the registry-only path when host discovery is disabled", async () => {
    process.env.PHASEGENT_WORKTREE_NO_DISCOVER = "1";
    rememberWorktree("parent-1", WORKTREE);
    rememberWorktree("child-1", WORKTREE);
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

    // A registered session still moves, with no parentage lookup and no CLI.
    await expect(prompt(promptEvent("child-1"))).resolves.toBeUndefined();
    expect(moves).toEqual([{ sessionID: "child-1", directory: WORKTREE }]);
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
    const context = {
      location: { directory: "/repo" },
      session: { move: async (input) => moves.push(input) },
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

describe("agentRole (issue #541)", () => {
  test("maps known agent names to phasegent roles", () => {
    expect(agentRole({ agent: "orchestrator" })).toBe("orchestrator");
    expect(agentRole({ agent: "executor" })).toBe("executor");
    expect(agentRole({ agent: "reviewer" })).toBe("reviewer");
    expect(agentRole({ agent: "tester" })).toBe("tester");
    // explore is read-only recon and behaves as a reviewer.
    expect(agentRole({ agent: "explore" })).toBe("reviewer");
  });

  test("is case-insensitive and matches compound agent names", () => {
    expect(agentRole({ agent: "Executor" })).toBe("executor");
    expect(agentRole({ agent: "task-executor" })).toBe("executor");
  });

  test("never guesses for unknown or missing agents", () => {
    expect(agentRole({ agent: "general" })).toBeNull();
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
        agent: "tester",
      }),
    ).toBe("PHASEGENT_ROLE=tester phasegent issue status");
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
    const definition = worktreeStrategyDefinition({
      issueId: 532,
      directory: "/repo",
      acquire: async () => null,
      gitAdd: async (input) => {
        fallbacks.push(input);
        return { directory: input.directory };
      },
      readLeases: async () => [],
    });
    const input = { sourceDirectory: "/repo", directory: "/wt/new", branch: "main" };
    expect(await definition.create(input)).toEqual({ directory: "/wt/new" });
    expect(fallbacks).toEqual([input]);
  });

  test("remove never deletes a worktree or branch", async () => {
    const definition = worktreeStrategyDefinition({
      issueId: 532,
      directory: "/repo",
      acquire: async () => null,
      gitAdd: async () => ({ directory: "/wt/new" }),
      readLeases: async () => [],
    });
    expect(await definition.remove({ directory: WORKTREE, force: true })).toBeUndefined();
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
      "phasegent-tester",
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
    // The generic skill plus the five role variants are what setup registers.
    expect(skillDefinitions()).toHaveLength(6);
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
    expect(explore).toContain("You publish no audit note, marker, or VERDICT");
    const shared = flat(skillDefinition().content);
    expect(shared).toContain("Nested explorer assistance changes no contract");
    expect(shared).toContain("stays read-only, non-audited, and unable to recurse");
  });

  test("the embedded tester prompt carries the test-only verification protocol (issue 679)", () => {
    // The tester role ships its own slim skill: the tester marker, the
    // test-only write boundary, independent verification, and the shared status
    // vocabulary plus explicit test-result evidence — never a new verdict token.
    const flat = (content) => content.split(/\s+/).join(" ");
    const tester = flat(
      roleSkillDefinitions().find((definition) => definition.id === "phasegent-tester").content,
    );
    expect(tester).toContain("<!-- ai-tester issue=");
    expect(tester).toContain(
      "Write only the test, fixture, and harness paths the orchestrator allowlists",
    );
    expect(tester).toContain("Never modify production code");
    expect(tester).toContain("the exact commands run, the observed pass/fail outcome");
    expect(tester).toContain("`DONE`/`PARTIAL`/`BLOCKED`/`FAILED`");
    expect(tester).toContain("no new verdict token");
    expect(tester).not.toContain("AUDIT_FAILED");
    expect(tester).not.toContain("REQUEST_CHANGES");
  });

  test("the embedded prompts carry the risk-based review policy (issue 679 P3)", () => {
    // The shared and role prompts define one risk class per phase, a
    // `reviewer_policy` that defaults to `final-only` and only escalates to
    // `checkpoint-and-final` for a planned checkpoint, an executor test
    // disposition, compact evidence, and serial-by-default overlap that never
    // allows overlapping write owners.
    const flat = (content) => content.split(/\s+/).join(" ");
    const contentFor = (id) =>
      roleSkillDefinitions().find((definition) => definition.id === id).content;
    const shared = flat(skillDefinition().content);
    expect(shared).toContain("## Risk classes and reviewer policy");
    expect(shared).toContain("`final-only` is the default for `standard` work");
    expect(shared).toContain("a checkpoint review never replaces the final one");
    expect(shared).toContain("## Bounded parallelism (serial by default)");
    expect(shared).toContain("Overlapping write owners are never allowed");
    expect(shared).toContain("## Test disposition and compact evidence");
    const orchestrator = flat(contentFor("phasegent-orchestrator"));
    expect(orchestrator).toContain("`reviewer_policy`");
    expect(orchestrator).toContain(
      "`checkpoint-and-final` is allowed only for `high-risk` or `irreversible` work",
    );
    expect(orchestrator).toContain("Keep orchestration serial by default");
    const executor = flat(contentFor("phasegent-executor"));
    expect(executor).toContain("Declare a test disposition in your note");
    expect(executor).toContain("You remain the only write owner for the phase");
    const reviewer = flat(contentFor("phasegent-reviewer"));
    expect(reviewer).toContain("The default is one `final-only` audit of `standard` work");
    expect(reviewer).toContain("do not repeat the tester report");
    expect(reviewer).toContain("`REVIEW:` line beside the `VERDICT:` line");
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
      "phasegent-tester",
      "phasegent-explore",
    ]);
    const skill = skills.get("phasegent");
    expect(skill.path).toBe("/builtin/phasegent.md");
    expect(skill.content).toContain("# Phasegent");
    for (const id of [
      "phasegent-orchestrator",
      "phasegent-executor",
      "phasegent-reviewer",
      "phasegent-tester",
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
    ["tester", "phasegent-tester"],
    ["explore", "phasegent-explore"],
  ];

  function agentState() {
    return [
      { id: "orchestrator", system: "You own the objective." },
      { id: "executor", system: "You implement a phase." },
      { id: "reviewer", system: "You review a phase." },
      { id: "explore", system: "You recon." },
      { id: "tester", system: "You test." },
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
    // The tester agent carries its own tester skill like the other protocol
    // agents.
    expect(
      state
        .find((entry) => entry.id === "tester")
        .system.startsWith(roleSkillContent("phasegent-tester")),
    ).toBe(true);
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
    expect(roleSkillId("tester")).toBe("phasegent-tester");
    expect(roleSkillId("build")).toBeNull();
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

// ---------------------------------------------------------------------------
// The phasegent MCP server registration.
//
// The registration adds the local `phasegent mcp serve` entry to the host MCP
// state so the contracted tracking tools are reachable. Its tests are about
// additive, non-clobbering behavior: the entry is never duplicated, an
// operator's own entry wins, and every refused, unrecognised, or failing host
// surface is a warning with no registration rather than a throw.
// ---------------------------------------------------------------------------

describe("phasegent MCP server registration", () => {
  afterEach(() => {
    forgetMcpRegistration();
  });

  // The v2 host MCP draft: `{ list, get, set, update, remove }` over the
  // server map, which is the shape the host's own config plugin writes through.
  function mcpDraft(seed) {
    const servers = new Map(Object.entries(seed || {}));
    return {
      servers,
      list: () => [...servers.entries()],
      get: (name) => servers.get(name),
      set: (name, entry) => servers.set(name, entry),
      update: (name, mutate) => {
        if (servers.has(name)) mutate(servers.get(name));
      },
      remove: (name) => servers.delete(name),
    };
  }

  test("the server entry is the v2 local CLI with a fixed least-privilege role", () => {
    const definition = phasegentMcpServerDefinition();
    // The v2 server shape: `mcp.servers.<name>` holds this object, and
    // `disabled` is the flag that keeps the host connected to it — an entry
    // that left it out is not a server the host will start.
    expect(definition).toEqual({
      type: "local",
      command: ["phasegent", "mcp", "serve", "--transport", "stdio"],
      environment: { PHASEGENT_ROLE: "executor" },
      disabled: false,
    });
    expect("enabled" in definition).toBe(false);
    expect(MCP_SERVER_ROLE).toBe("executor");
    // A fresh object per call, so a host that mutates it cannot poison the next.
    expect(phasegentMcpServerDefinition()).not.toBe(definition);
    definition.command.push("--authorized");
    expect(phasegentMcpServerDefinition().command).not.toContain("--authorized");
  });

  test("the draft gains the entry, keeps every other one, and is never clobbered", () => {
    const mine = phasegentMcpServerDefinition();
    const other = { type: "remote", url: "https://example.test/mcp" };
    const draft = mcpDraft({ other });
    expect(hasPhasegentServer(draft)).toBe(false);
    expect(applyServer(draft, mine)).toBe(true);
    expect(draft.get(PHASEGENT_MCP_SERVER)).toEqual(mine);
    expect(draft.get("other")).toBe(other);
    expect(draft.list().map(([name]) => name).sort()).toEqual(["other", "phasegent"]);
    // A second registration with a different definition is a no-op: the entry
    // the host already has wins, exactly like the host's own config plugin.
    expect(applyServer(draft, { type: "local", command: ["operator", "choice"] })).toBe(true);
    expect(draft.get(PHASEGENT_MCP_SERVER)).toEqual(mine);
    // An operator's own phasegent entry is left byte-for-byte alone.
    const configured = { type: "local", command: ["pinned"], disabled: true };
    const seeded = mcpDraft({ phasegent: configured });
    expect(applyServer(seeded, mine)).toBe(true);
    expect(seeded.get(PHASEGENT_MCP_SERVER)).toBe(configured);
    // A draft this bridge does not understand never claims a registration.
    expect(applyServer({ set: () => {} }, mine)).toBe(false);
    expect(applyServer(null, mine)).toBe(false);
  });

  test("the same entry is found in a v2 config object and in the host draft", () => {
    const mine = phasegentMcpServerDefinition();
    // The canonical chezmoi shape, which carries unrelated servers too.
    const config = { mcp: { servers: { cloudiful: { type: "remote" }, phasegent: mine } } };
    expect(hasPhasegentServer(config)).toBe(true);
    expect(hasPhasegentServer({ mcp: { servers: { cloudiful: { type: "remote" } } } })).toBe(false);
    expect(hasPhasegentServer({ mcp: {} })).toBe(false);
    expect(hasPhasegentServer({})).toBe(false);
    expect(hasPhasegentServer(null)).toBe(false);
    // `mcp.servers.<name>` is the only place the entry lives, so a config that
    // nests it directly under `mcp` does not count as configured.
    expect(hasPhasegentServer({ mcp: { phasegent: mine } })).toBe(false);
  });

  test("the host mcp domain registers the entry and then reloads", async () => {
    const other = { type: "remote", url: "https://example.test/mcp" };
    const draft = mcpDraft({ other });
    const reloaded = [];
    const registration = await registerPhasegentMcp({
      mcp: {
        transform: async (mutate) => {
          mutate(draft);
          return { dispose: async () => {} };
        },
        reload: async () => {
          reloaded.push(PHASEGENT_MCP_SERVER);
        },
      },
    });
    expect(registration).toEqual({ registered: true, reason: null });
    expect(mcpRegistered()).toBe(true);
    expect(draft.get(PHASEGENT_MCP_SERVER)).toEqual(phasegentMcpServerDefinition());
    expect(draft.get("other")).toBe(other);
    // The host connects an added server on a domain reload, so the registration
    // ends with one, exactly as the host's own config plugin does.
    expect(reloaded).toEqual([PHASEGENT_MCP_SERVER]);

    forgetMcpRegistration();
    const configured = { type: "local", command: ["pinned"], disabled: false };
    const alreadyThere = mcpDraft({ phasegent: configured });
    expect(
      await registerPhasegentMcp({
        mcp: { transform: async (mutate) => mutate(alreadyThere) },
      }),
    ).toEqual({ registered: true, reason: null });
    expect(alreadyThere.get(PHASEGENT_MCP_SERVER)).toBe(configured);
  });

  test("an absent, unrecognised, or failing MCP surface is a warning and no registration", async () => {
    for (const context of [{}, { mcp: {} }, { mcp: { transform: 42 } }]) {
      expect(await registerPhasegentMcp(context)).toMatchObject({
        registered: false,
        reason: "no-mcp-transform",
      });
      expect(mcpRegistered()).toBe(false);
    }
    // A transform that never reaches the callback, and a draft without the two
    // calls the host's own config plugin makes, both leave the host state
    // without the entry — so the tracking tools stay unavailable.
    expect(
      await registerPhasegentMcp({ mcp: { transform: async () => ({ dispose: async () => {} }) } }),
    ).toMatchObject({ registered: false, reason: "entry-not-confirmed" });
    expect(
      await registerPhasegentMcp({ mcp: { transform: async (mutate) => mutate({ set: () => {} }) } }),
    ).toMatchObject({ registered: false, reason: "entry-not-confirmed" });
    // A host that rejects the transform, and one that rejects the entry itself,
    // are both warnings rather than a throw: a throw here would disable the
    // whole plugin, redirect hook included.
    expect(
      await registerPhasegentMcp({
        mcp: {
          transform: async () => {
            throw new Error("host rejected the transform");
          },
        },
      }),
    ).toMatchObject({ registered: false, reason: "mcp-transform-failed" });
    const refused = mcpDraft({});
    refused.set = () => {
      throw new Error("host rejected the entry");
    };
    expect(
      await registerPhasegentMcp({ mcp: { transform: async (mutate) => mutate(refused) } }),
    ).toMatchObject({ registered: false, reason: "entry-not-confirmed" });
    expect(mcpRegistered()).toBe(false);
  });

  test("a rejected reload keeps the entry", async () => {
    const draft = mcpDraft({});
    const registration = await registerPhasegentMcp({
      mcp: {
        transform: async (mutate) => mutate(draft),
        reload: async () => {
          throw new Error("reload unavailable");
        },
      },
    });
    // The host state carries the entry, which is the same judgement the host's
    // own config plugin stops at; a failed connect is a warning, not a refusal.
    expect(registration).toMatchObject({ registered: true });
    expect(draft.get(PHASEGENT_MCP_SERVER)).toEqual(phasegentMcpServerDefinition());
    expect(mcpRegistered()).toBe(true);
  });
});
