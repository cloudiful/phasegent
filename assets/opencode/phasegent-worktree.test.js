// Focused tests for the phasegent worktree plugin (issue #532, v2 contract).
//
// The helpers are pure or dependency-injected so they run under `bun test`
// without OpenCode and without the phasegent CLI: the plugin module is imported
// directly and only its default export plus the attached `redirect` helpers are
// touched. The install/status/uninstall marker behaviour is covered by the Rust
// asset assertions in `src/plugin_tests.rs`.

import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import PhasegentWorktreePlugin from "./phasegent-worktree.js";

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
  pickActiveWorktreePath,
  registerWorktreeStrategy,
  worktreeStrategyDefinition,
  registerWorktreeSkill,
  worktreeSkillDefinition,
} = PhasegentWorktreePlugin.redirect;

const WORKTREE = "/repo/.worktrees/issue-532";

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

  test("setup registers the tool hook and skill, and returns a cleanup", async () => {
    const saved = process.env.PHASEGENT_WORKTREE_NO_DISCOVER;
    process.env.PHASEGENT_WORKTREE_NO_DISCOVER = "1";
    try {
      const registered = [];
      const context = {
        location: { directory: "/repo" },
        session: { move: async () => {} },
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
      };
      const cleanup = await PhasegentWorktreePlugin.setup(context);
      expect(registered.map((item) => item.name)).toEqual(["execute.before", "skill.transform"]);
      expect(typeof registered[0].callback).toBe("function");
      expect(typeof cleanup).toBe("function");
      await cleanup();
    } finally {
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

describe("tool.execute.before hook (v2 single event)", () => {
  let savedNoDiscover;
  beforeEach(() => {
    savedNoDiscover = process.env.PHASEGENT_WORKTREE_NO_DISCOVER;
    process.env.PHASEGENT_WORKTREE_NO_DISCOVER = "1";
  });
  afterEach(() => {
    restoreNoDiscover(savedNoDiscover);
  });

  test("mutates event.input for a relative read", async () => {
    rememberWorktree("session-1", WORKTREE);
    const hook = createRedirectHook();
    const event = { tool: "read", sessionID: "session-1", input: { path: "src/a.rs" } };
    await hook(event);
    expect(event.input.path).toBe(`${WORKTREE}/src/a.rs`);
    expect(event.tool).toBe("read");
  });

  test("redirects a bare shell cwd for a sub-agent session", async () => {
    rememberWorktree("parent-session", WORKTREE);
    const hook = createRedirectHook();
    const event = { tool: "shell", sessionID: "child-session", input: { command: "pwd" } };
    await hook(event);
    expect(event.input.workdir).toBe(WORKTREE);
  });

  test("leaves absolute paths and no-worktree sessions unchanged", async () => {
    const hook = createRedirectHook();
    const noWorktree = { tool: "read", sessionID: "session-1", input: { path: "src/a.rs" } };
    await hook(noWorktree);
    expect(noWorktree.input.path).toBe("src/a.rs");

    rememberWorktree("session-1", WORKTREE);
    const absolute = { tool: "read", sessionID: "session-1", input: { path: "/etc/hosts" } };
    await hook(absolute);
    expect(absolute.input.path).toBe("/etc/hosts");
  });

  test("ignores calls without input", async () => {
    rememberWorktree("session-1", WORKTREE);
    const hook = createRedirectHook();
    await hook({ tool: "shell", sessionID: "session-1" });
  });

  test("moves the session once and skips path redirection once placed", async () => {
    rememberWorktree("session-1", WORKTREE);
    const moves = [];
    const context = {
      location: { directory: "/repo" },
      session: { move: async (input) => moves.push(input) },
    };
    const hook = createRedirectHook(context);
    const first = { tool: "read", sessionID: "session-1", input: { path: "src/a.rs" } };
    await hook(first);
    // The call that triggered the move still runs in the old cwd.
    expect(first.input.path).toBe(`${WORKTREE}/src/a.rs`);
    expect(sessionPlaced("session-1")).toBe(true);

    const second = { tool: "read", sessionID: "session-1", input: { path: "src/b.rs" } };
    await hook(second);
    // The session cwd now is the worktree, so the relative path is left alone.
    expect(second.input.path).toBe("src/b.rs");
    expect(moves).toEqual([{ sessionID: "session-1", directory: WORKTREE }]);
  });

  test("survives a failing session move", async () => {
    rememberWorktree("session-1", WORKTREE);
    const context = {
      location: { directory: "/repo" },
      session: {
        move: async () => {
          throw new Error("destination unavailable");
        },
      },
    };
    const hook = createRedirectHook(context);
    const event = { tool: "read", sessionID: "session-1", input: { path: "src/a.rs" } };
    await hook(event);
    // A failed move is not a placement: the fallback path rewrite still runs.
    expect(event.input.path).toBe(`${WORKTREE}/src/a.rs`);
    expect(sessionPlaced("session-1")).toBe(false);
  });

  test("rewrites paths when the host has no session.move", async () => {
    rememberWorktree("session-1", WORKTREE);
    const hook = createRedirectHook({ location: { directory: "/repo" } });
    const event = { tool: "read", sessionID: "session-1", input: { path: "src/a.rs" } };
    await hook(event);
    expect(event.input.path).toBe(`${WORKTREE}/src/a.rs`);
    expect(sessionPlaced("session-1")).toBe(false);
  });

  test("still rewrites commands for an already placed session", async () => {
    rememberWorktree("session-1", WORKTREE);
    const context = {
      location: { directory: "/repo" },
      session: { move: async () => {} },
    };
    const hook = createRedirectHook(context);
    await hook({ tool: "read", sessionID: "session-1", input: { path: "src/a.rs" } });
    expect(sessionPlaced("session-1")).toBe(true);

    const event = {
      tool: "shell",
      sessionID: "session-1",
      agent: "orchestrator",
      input: { command: "phasegent issue get 1" },
    };
    await hook(event);
    // Command rewriting is independent of the placement fast path.
    expect(event.input.command).toBe("phasegent --role orchestrator issue get 1");
    expect(event.input.workdir).toBeUndefined();
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

  test("resetWorktrees clears move attempts and placements", async () => {
    rememberWorktree("session-1", WORKTREE);
    const moves = [];
    const context = {
      location: { directory: "/repo" },
      session: { move: async (input) => moves.push(input) },
    };
    const hook = createRedirectHook(context);
    await hook({ tool: "read", sessionID: "session-1", input: { path: "src/a.rs" } });
    expect(sessionPlaced("session-1")).toBe(true);

    resetWorktrees();
    expect(sessionPlaced("session-1")).toBe(false);

    rememberWorktree("session-1", WORKTREE);
    await hook({ tool: "read", sessionID: "session-1", input: { path: "src/b.rs" } });
    // The attempt set was cleared too, so a fresh move is allowed.
    expect(moves).toHaveLength(2);
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
      "phasegent --role orchestrator issue create --title t --body b --session session-1 | tee /tmp/x",
    );
  });

  test("keeps an existing --role and appends --session to issue bind", () => {
    expect(
      rewritePhasegentCommand(
        "phasegent --role executor --provider local issue bind 18",
        "abc",
        { agent: "orchestrator" },
      ),
    ).toBe("phasegent --role executor --provider local issue bind 18 --session abc");
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

  test("injects --role right after the phasegent token", () => {
    expect(
      rewritePhasegentCommand("phasegent issue status", "s1", { agent: "executor" }),
    ).toBe("phasegent --role executor issue status");
    expect(
      rewritePhasegentCommand("phasegent worktree status --issue 1", undefined, {
        agent: "reviewer",
      }),
    ).toBe("phasegent --role reviewer worktree status --issue 1");
  });

  test("leaves an existing --role flag untouched", () => {
    expect(
      rewritePhasegentCommand("phasegent --role reviewer issue get 1", "s1", {
        agent: "executor",
      }),
    ).toBe("phasegent --role reviewer issue get 1");
    expect(
      rewritePhasegentCommand("phasegent --role=reviewer issue get 1", "s1", {
        agent: "executor",
      }),
    ).toBe("phasegent --role=reviewer issue get 1");
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

  test("appends --session only to an issue create/bind segment", () => {
    expect(
      rewritePhasegentCommand("phasegent issue status", "s1", undefined),
    ).toBe("phasegent issue status");
    expect(rewritePhasegentCommand("ls -la", "s1", undefined)).toBe("ls -la");
    expect(
      rewritePhasegentCommand("phasegent worktree acquire --issue 18", "s1", undefined),
    ).toBe("phasegent worktree acquire --issue 18");
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
      "phasegent --role orchestrator issue get 1 && phasegent --role orchestrator issue bind 541 --session s9",
    );
  });

  test("rewrites an invocation behind env assignments or a path prefix", () => {
    expect(
      rewritePhasegentCommand(
        "PHASEGENT_WORKTREE_NO_DISCOVER=1 phasegent issue status",
        "s1",
        { agent: "executor" },
      ),
    ).toBe("PHASEGENT_WORKTREE_NO_DISCOVER=1 phasegent --role executor issue status");
    expect(
      rewritePhasegentCommand("./phasegent issue status", "s1", { agent: "executor" }),
    ).toBe("./phasegent --role executor issue status");
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
      "phasegent --role orchestrator issue create --title t",
    ]) {
      const out = rewritePhasegentCommand(command, "s1", { agent: "executor" });
      expect(out).toContain("cannot run 'issue create|bind'");
      expect(out.endsWith("; false")).toBe(true);
      expect(out).not.toContain("phasegent issue create");
      expect(out).not.toContain("--title");
    }
  });

  test("downgrades a claimed orchestrator/admin role to the session role", () => {
    expect(
      rewritePhasegentCommand("phasegent --role orchestrator issue close 1", "s1", {
        agent: "executor",
      }),
    ).toBe("phasegent --role executor issue close 1");
    expect(
      rewritePhasegentCommand("phasegent --role=admin issue status", "s1", {
        agent: "tester",
      }),
    ).toBe("phasegent --role=tester issue status");
  });

  test("downgrades a PHASEGENT_ROLE=orchestrator|admin env claim", () => {
    expect(
      rewritePhasegentCommand(
        "PHASEGENT_ROLE=orchestrator phasegent worktree acquire --issue 1",
        "s1",
        { agent: "executor" },
      ),
    ).toBe(
      "PHASEGENT_ROLE=executor phasegent --role executor worktree acquire --issue 1",
    );
    expect(
      rewritePhasegentCommand(
        "FOO=1 PHASEGENT_ROLE=admin phasegent issue get 1",
        "s1",
        { agent: "reviewer" },
      ),
    ).toBe("FOO=1 PHASEGENT_ROLE=reviewer phasegent --role reviewer issue get 1");
  });

  test("keeps an orchestrator session's own role claim", () => {
    expect(
      rewritePhasegentCommand("phasegent --role orchestrator issue close 1", "s1", {
        agent: "orchestrator",
      }),
    ).toBe("phasegent --role orchestrator issue close 1");
  });
});

describe("quote-aware command rewriting (issue #541 P1)", () => {
  test("keeps --session outside a quoted pipe in --body", () => {
    expect(
      rewritePhasegentCommand('phasegent issue create --title t --body "a | b"', "session-1", {
        agent: "orchestrator",
      }),
    ).toBe(
      'phasegent --role orchestrator issue create --title t --body "a | b" --session session-1',
    );
  });

  test("keeps --session outside a multiline --body", () => {
    // A real newline inside the value must not split the segment.
    const command = 'phasegent issue create --title t --body "l1\nl2"';
    expect(rewritePhasegentCommand(command, "session-1", { agent: "orchestrator" })).toBe(
      'phasegent --role orchestrator issue create --title t --body "l1\nl2" --session session-1',
    );
  });

  test("keeps --session outside a quoted && in --title", () => {
    expect(
      rewritePhasegentCommand('phasegent issue create --title "a && b"', "session-1", {
        agent: "orchestrator",
      }),
    ).toBe(
      'phasegent --role orchestrator issue create --title "a && b" --session session-1',
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
      'phasegent --role orchestrator issue create --title t --body "--session s9" --session session-1',
    );
  });

  test("does not mistake a quoted issue bind for an issue write", () => {
    expect(
      rewritePhasegentCommand('phasegent issue get 1 --body "issue bind"', "session-1", {
        agent: "executor",
      }),
    ).toBe('phasegent --role executor issue get 1 --body "issue bind"');
  });

  test("does not rewrite a quoted --role value for a sub-agent", () => {
    expect(
      rewritePhasegentCommand('phasegent issue get 1 --body "--role orchestrator"', "s1", {
        agent: "executor",
      }),
    ).toBe('phasegent --role executor issue get 1 --body "--role orchestrator"');
  });

  test("does not rewrite a quoted PHASEGENT_ROLE value for a sub-agent", () => {
    expect(
      rewritePhasegentCommand(
        'phasegent issue get 1 --body "PHASEGENT_ROLE=orchestrator"',
        "s1",
        { agent: "executor" },
      ),
    ).toBe('phasegent --role executor issue get 1 --body "PHASEGENT_ROLE=orchestrator"');
  });

  test("handles single-quoted values and escaped quotes", () => {
    expect(
      rewritePhasegentCommand(
        "phasegent issue create --title t --body 'a | b'",
        "s1",
        { agent: "orchestrator" },
      ),
    ).toBe("phasegent --role orchestrator issue create --title t --body 'a | b' --session s1");
    expect(
      rewritePhasegentCommand(
        'phasegent issue create --title t --body "a \\" | b"',
        "s1",
        { agent: "orchestrator" },
      ),
    ).toBe(
      'phasegent --role orchestrator issue create --title t --body "a \\" | b" --session s1',
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
      "phasegent --role orchestrator issue create --title t --body b --session session-1 | tee /tmp/x",
    );
    expect(
      rewritePhasegentCommand(
        'phasegent issue bind 541 --note "a && b" | tee /tmp/y',
        "s9",
        { agent: "orchestrator" },
      ),
    ).toBe(
      'phasegent --role orchestrator issue bind 541 --note "a && b" --session s9 | tee /tmp/y',
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
      'phasegent --role executor issue status; echo "a | b"; phasegent --role executor worktree status',
    );
  });

  test("still recognises a quoted env value before the invocation", () => {
    expect(
      rewritePhasegentCommand(
        'PHASEGENT_WORKTREE_NO_DISCOVER="1" phasegent issue status',
        "s1",
        { agent: "executor" },
      ),
    ).toBe('PHASEGENT_WORKTREE_NO_DISCOVER="1" phasegent --role executor issue status');
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
      input: { command: "phasegent --role orchestrator issue status", workdir: "/tmp" },
    };
    await hook(event);
    expect(event.input.command).toBe("phasegent --role executor issue status");
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

  test("create returns the acquired lease path", async () => {
    const definition = worktreeStrategyDefinition({
      issueId: 532,
      directory: "/repo",
      acquire: async (issueId, sessionId, cwd) => {
        expect(issueId).toBe(532);
        expect(sessionId).toBeNull();
        expect(cwd).toBe("/repo");
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

describe("v2 skill.transform (embedded phasegent-worktree-v2)", () => {
  test("definition is the flat Skill.Info the v2.0.11 host draft accepts", () => {
    const definition = worktreeSkillDefinition();
    expect(definition.id).toBe("phasegent-worktree-v2");
    expect(definition.name).toBe("phasegent-worktree-v2");
    expect(definition.path).toBe("/builtin/phasegent-worktree-v2.md");
    expect(definition.description).toContain("PHASEGENT_SESSION_ID");
    expect(definition.content).toContain("phasegent worktree prune");
    expect(definition.content.startsWith("---\nname: phasegent-worktree-v2\n")).toBe(true);
    // The removed SDK draft shape must not come back: the runtime `add` takes
    // the flat info, not a `{ type: "embedded", skill }` source.
    expect(definition.type).toBeUndefined();
    expect(definition.skill).toBeUndefined();
  });

  test("embedded content matches skills/phasegent-worktree-v2/SKILL.md", async () => {
    const path = new URL("../../skills/phasegent-worktree-v2/SKILL.md", import.meta.url);
    const disk = await Bun.file(path).text();
    expect(worktreeSkillDefinition().content).toBe(disk);
  });

  test("registerWorktreeSkill adds the info through the runtime draft", async () => {
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
    const registration = await registerWorktreeSkill(context);
    expect(registration).toBeObject();
    expect(skills.size).toBe(1);
    const skill = skills.get("phasegent-worktree-v2");
    expect(skill.path).toBe("/builtin/phasegent-worktree-v2.md");
    expect(skill.content).toContain("# phasegent worktree (OpenCode v2 adapter)");
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
      const registration = await registerWorktreeSkill(context);
      expect(registration).toBeObject();
      expect(warnings.some((line) => line.includes("exposes no add"))).toBe(true);
    } finally {
      console.warn = original;
    }
  });

  test("returns null when the host exposes no skill.transform", async () => {
    expect(await registerWorktreeSkill({})).toBeNull();
    expect(await registerWorktreeSkill(undefined)).toBeNull();
  });
});
