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
  redirectArgs,
  rememberWorktree,
  worktreeForSession,
  resetWorktrees,
  createRedirectHook,
  injectSessionIntoPhasegentCommand,
  pickActiveWorktreePath,
  registerWorktreeStrategy,
  worktreeStrategyDefinition,
  registerWorktreeCommand,
  worktreeAcquireCommandTemplate,
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

  test("setup registers the tool hook, command and skill, and returns a cleanup", async () => {
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
        command: {
          transform: async () => {
            registered.push({ name: "command.transform" });
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
      expect(registered.map((item) => item.name)).toEqual([
        "execute.before",
        "command.transform",
        "skill.transform",
      ]);
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
      command: {
        transform: async () => {
          throw new Error("command transform unavailable");
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

describe("redirectArgs file tools (v2 `path` argument)", () => {
  test("redirects a relative path for read/write/edit", () => {
    for (const tool of ["read", "write", "edit"]) {
      const args = { path: "src/a.rs" };
      expect(redirectArgs(tool, WORKTREE, args).path).toBe(`${WORKTREE}/src/a.rs`);
    }
  });

  test("redirects a relative path for glob/grep and leaves the pattern", () => {
    for (const tool of ["glob", "grep"]) {
      const args = { pattern: "*.rs", path: "src" };
      const out = redirectArgs(tool, WORKTREE, args);
      expect(out.path).toBe(`${WORKTREE}/src`);
      expect(out.pattern).toBe("*.rs");
    }
  });

  test("defaults a missing glob/grep path to the worktree", () => {
    for (const tool of ["glob", "grep"]) {
      expect(redirectArgs(tool, WORKTREE, {}, "session-1").path).toBe(WORKTREE);
      expect(redirectArgs(tool, WORKTREE, { pattern: "*.rs" }, "session-1").path).toBe(
        WORKTREE,
      );
    }
  });

  test("defaults an empty or non-string glob/grep path to the worktree", () => {
    expect(redirectArgs("glob", WORKTREE, { path: "" }, "session-1").path).toBe(
      WORKTREE,
    );
    expect(redirectArgs("grep", WORKTREE, { path: "" }, "session-1").path).toBe(
      WORKTREE,
    );
    expect(redirectArgs("glob", WORKTREE, { path: 42 }, "session-1").path).toBe(
      WORKTREE,
    );
    expect(redirectArgs("grep", WORKTREE, { path: null }, "session-1").path).toBe(
      WORKTREE,
    );
  });

  test("joins relative glob/grep paths and passes absolute paths through", () => {
    for (const tool of ["glob", "grep"]) {
      expect(redirectArgs(tool, WORKTREE, { path: "src" }, "session-1").path).toBe(
        `${WORKTREE}/src`,
      );
      expect(
        redirectArgs(
          tool,
          WORKTREE,
          { path: "/home/dev/codes/tools/phasegent/src" },
          "session-1",
        ).path,
      ).toBe("/home/dev/codes/tools/phasegent/src");
    }
  });

  test("passes absolute file paths through unchanged", () => {
    const out = redirectArgs("read", WORKTREE, { path: "/etc/hosts" });
    expect(out.path).toBe("/etc/hosts");
  });

  test("does not touch other tools or non-string fields", () => {
    const out = redirectArgs("webfetch", WORKTREE, { url: "src/a.rs" });
    expect(out.url).toBe("src/a.rs");
  });
});

describe("redirectArgs shell workdir (v2 `shell` tool)", () => {
  test("fills a bare shell workdir with the worktree", () => {
    const out = redirectArgs("shell", WORKTREE, { command: "ls" });
    expect(out.workdir).toBe(WORKTREE);
    expect(out.command).toBe("ls");
  });

  test("keeps the v1 `bash` tool name as an alias", () => {
    const out = redirectArgs("bash", WORKTREE, { command: "ls" });
    expect(out.workdir).toBe(WORKTREE);
  });

  test("resolves a relative shell workdir against the worktree", () => {
    const out = redirectArgs("shell", WORKTREE, { command: "ls", workdir: "sub" });
    expect(out.workdir).toBe(`${WORKTREE}/sub`);
  });

  test("keeps an absolute shell workdir and never rewrites the command", () => {
    const out = redirectArgs("shell", WORKTREE, { command: "cd /tmp && ls", workdir: "/tmp" });
    expect(out.workdir).toBe("/tmp");
    expect(out.command).toBe("cd /tmp && ls");
  });
});

describe("redirectArgs pass-through", () => {
  test("returns the same object when no worktree is known", () => {
    const args = { path: "src/a.rs" };
    expect(redirectArgs("read", null, args)).toBe(args);
    expect(redirectArgs("shell", "", args)).toBe(args);
  });

  test("returns non-object args untouched", () => {
    expect(redirectArgs("read", WORKTREE, undefined)).toBeUndefined();
    expect(redirectArgs("read", WORKTREE, null)).toBeNull();
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

  test("moves the session to the acquired worktree once", async () => {
    rememberWorktree("session-1", WORKTREE);
    const moves = [];
    const context = {
      location: { directory: "/repo" },
      session: { move: async (input) => moves.push(input) },
    };
    const hook = createRedirectHook(context);
    await hook({ tool: "read", sessionID: "session-1", input: { path: "src/a.rs" } });
    await hook({ tool: "read", sessionID: "session-1", input: { path: "src/b.rs" } });
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
    expect(event.input.path).toBe(`${WORKTREE}/src/a.rs`);
  });
});

describe("injectSessionIntoPhasegentCommand (issue #18 Task 2)", () => {
  test("appends --session once to issue create", () => {
    const out = injectSessionIntoPhasegentCommand(
      "phasegent issue create --title t --body b",
      "session-1",
    );
    expect(out).toBe("phasegent issue create --title t --body b --session session-1");
  });

  test("appends --session once to issue bind", () => {
    const out = injectSessionIntoPhasegentCommand(
      "phasegent --role executor --provider local issue bind 18",
      "abc",
    );
    expect(out).toBe(
      "phasegent --role executor --provider local issue bind 18 --session abc",
    );
  });

  test("skips when --session is already present", () => {
    expect(
      injectSessionIntoPhasegentCommand(
        "phasegent issue create --title t --session s1",
        "s2",
      ),
    ).toBe("phasegent issue create --title t --session s1");
    expect(
      injectSessionIntoPhasegentCommand(
        "phasegent issue bind 18 --session=s1",
        "s2",
      ),
    ).toBe("phasegent issue bind 18 --session=s1");
  });

  test("skips non-create/bind commands", () => {
    expect(
      injectSessionIntoPhasegentCommand("phasegent issue status", "s1"),
    ).toBe("phasegent issue status");
    expect(injectSessionIntoPhasegentCommand("ls -la", "s1")).toBe("ls -la");
    expect(
      injectSessionIntoPhasegentCommand(
        "phasegent worktree acquire --issue 18",
        "s1",
      ),
    ).toBe("phasegent worktree acquire --issue 18");
  });

  test("passes through without a session id", () => {
    const command = "phasegent issue create --title t";
    expect(injectSessionIntoPhasegentCommand(command, undefined)).toBe(command);
    expect(injectSessionIntoPhasegentCommand(command, "")).toBe(command);
    expect(injectSessionIntoPhasegentCommand(undefined, "s1")).toBeUndefined();
  });
});

describe("redirectArgs session injection (issue #18 Task 2)", () => {
  test("injects --session into a shell phasegent issue create", () => {
    const out = redirectArgs(
      "shell",
      WORKTREE,
      { command: "phasegent issue create --title t --body b" },
      "session-1",
    );
    expect(out.command).toBe(
      "phasegent issue create --title t --body b --session session-1",
    );
    expect(out.workdir).toBe(WORKTREE);
  });

  test("does not duplicate --session and leaves absolute workdir alone", () => {
    const out = redirectArgs(
      "shell",
      WORKTREE,
      {
        command: "phasegent issue bind 18 --session s1",
        workdir: "/tmp",
      },
      "s2",
    );
    expect(out.command).toBe("phasegent issue bind 18 --session s1");
    expect(out.workdir).toBe("/tmp");
  });

  test("leaves non-phasegent shell commands alone", () => {
    const out = redirectArgs("shell", WORKTREE, { command: "ls" }, "s1");
    expect(out.command).toBe("ls");
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

describe("v2 command.transform (/phasegent-worktree-acquire)", () => {
  test("template runs the bound-issue acquire through the host shell", () => {
    const template = worktreeAcquireCommandTemplate();
    expect(template).toContain("phasegent --role orchestrator worktree acquire");
    expect(template).toContain("--format json");
    expect(template).toContain("phasegent --role executor issue status");
    expect(template).toContain("$ARGUMENTS");
    expect(template).toContain("phasegent worktree prune");
  });

  test("template keeps user input out of the shell command", () => {
    const template = worktreeAcquireCommandTemplate();
    const shell = template.match(/!`([^`]+)`/);
    expect(shell).not.toBeNull();
    expect(shell[1]).not.toContain("$ARGUMENTS");
    expect(shell[1]).not.toContain("$1");
    expect(shell[1]).not.toContain("${");
  });

  test("registerWorktreeCommand updates an absent command in place", async () => {
    const commands = new Map();
    const draft = {
      list: () => [...commands.values()],
      get: (name) => commands.get(name),
      update: (name, mutate) => {
        const current = commands.get(name) ?? { name, template: "" };
        commands.set(name, current);
        mutate(current);
        current.name = name;
      },
      remove: (name) => commands.delete(name),
    };
    const context = {
      command: {
        transform: async (callback) => {
          // create-when-absent semantics of packages/core/src/command.ts
          await callback(draft);
          return { dispose: async () => {} };
        },
      },
    };
    const registration = await registerWorktreeCommand(context);
    expect(registration).toBeObject();
    const command = commands.get("phasegent-worktree-acquire");
    expect(command).toBeDefined();
    expect(command.name).toBe("phasegent-worktree-acquire");
    expect(command.template).toContain("worktree acquire");
    expect(command.description).toContain("worktree");
  });

  test("returns null when the host exposes no command.transform", async () => {
    expect(await registerWorktreeCommand({})).toBeNull();
    expect(await registerWorktreeCommand(undefined)).toBeNull();
  });
});

describe("v2 skill.transform (embedded phasegent-worktree-v2)", () => {
  test("definition is an embedded source with name, location and content", () => {
    const definition = worktreeSkillDefinition();
    expect(definition.type).toBe("embedded");
    expect(definition.skill.name).toBe("phasegent-worktree-v2");
    expect(definition.skill.location).toBe("/builtin/phasegent-worktree-v2.md");
    expect(definition.skill.description).toContain("PHASEGENT_SESSION_ID");
    expect(definition.skill.content).toContain("phasegent worktree prune");
    expect(definition.skill.content.startsWith("---\nname: phasegent-worktree-v2\n")).toBe(
      true,
    );
  });

  test("embedded content matches skills/phasegent-worktree-v2/SKILL.md", async () => {
    const path = new URL("../../skills/phasegent-worktree-v2/SKILL.md", import.meta.url);
    const disk = await Bun.file(path).text();
    expect(worktreeSkillDefinition().skill.content).toBe(disk);
  });

  test("registerWorktreeSkill registers the embedded source", async () => {
    const sources = [];
    const context = {
      skill: {
        transform: async (callback) => {
          await callback({
            source: (source) => sources.push(source),
            list: () => sources,
          });
          return { dispose: async () => {} };
        },
      },
    };
    const registration = await registerWorktreeSkill(context);
    expect(registration).toBeObject();
    expect(sources).toHaveLength(1);
    expect(sources[0].skill.name).toBe("phasegent-worktree-v2");
  });

  test("returns null when the host exposes no skill.transform", async () => {
    expect(await registerWorktreeSkill({})).toBeNull();
    expect(await registerWorktreeSkill(undefined)).toBeNull();
  });
});
