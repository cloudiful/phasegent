// Focused tests for the phasegent worktree plugin redirect helpers (issue #440).
//
// The helpers are pure so they run under `bun test` without OpenCode: the
// plugin module is imported directly and only its exported entry point plus the
// attached `redirect` helpers are touched. The workspace adapter / acquire chain
// is covered by the Rust asset assertions in `src/plugin_tests.rs`.

import { beforeEach, describe, expect, test } from "bun:test";
import PhasegentWorktreePlugin, {
  PhasegentWorktreePlugin as namedPlugin,
} from "./phasegent-worktree.js";

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
} = PhasegentWorktreePlugin.redirect;

const WORKTREE = "/repo/.worktrees/issue-440";

beforeEach(() => {
  resetWorktrees();
});

describe("plugin module shape", () => {
  test("named and default exports are the same plugin function", () => {
    expect(typeof PhasegentWorktreePlugin).toBe("function");
    expect(namedPlugin).toBe(PhasegentWorktreePlugin);
    expect(PhasegentWorktreePlugin.redirect).toBeObject();
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

describe("redirectArgs file tools", () => {
  test("redirects a relative filePath for read/write/edit", () => {
    for (const tool of ["read", "write", "edit"]) {
      const args = { filePath: "src/a.rs" };
      expect(redirectArgs(tool, WORKTREE, args).filePath).toBe(`${WORKTREE}/src/a.rs`);
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

  test("leaves an omitted glob/grep path omitted", () => {
    const out = redirectArgs("glob", WORKTREE, { pattern: "*.rs" });
    expect(out.path).toBeUndefined();
  });

  test("passes absolute file paths through unchanged", () => {
    const out = redirectArgs("read", WORKTREE, { filePath: "/etc/hosts" });
    expect(out.filePath).toBe("/etc/hosts");
  });

  test("does not touch other tools or non-string fields", () => {
    const out = redirectArgs("webfetch", WORKTREE, { url: "src/a.rs" });
    expect(out.url).toBe("src/a.rs");
  });
});

describe("redirectArgs bash workdir", () => {
  test("fills a bare bash workdir with the worktree", () => {
    const out = redirectArgs("bash", WORKTREE, { command: "ls" });
    expect(out.workdir).toBe(WORKTREE);
    expect(out.command).toBe("ls");
  });

  test("resolves a relative bash workdir against the worktree", () => {
    const out = redirectArgs("bash", WORKTREE, { command: "ls", workdir: "sub" });
    expect(out.workdir).toBe(`${WORKTREE}/sub`);
  });

  test("keeps an absolute bash workdir and never rewrites the command", () => {
    const out = redirectArgs("bash", WORKTREE, { command: "cd /tmp && ls", workdir: "/tmp" });
    expect(out.workdir).toBe("/tmp");
    expect(out.command).toBe("cd /tmp && ls");
  });
});

describe("redirectArgs pass-through", () => {
  test("returns the same object when no worktree is known", () => {
    const args = { filePath: "src/a.rs" };
    expect(redirectArgs("read", null, args)).toBe(args);
    expect(redirectArgs("bash", "", args)).toBe(args);
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

describe("tool.execute.before hook", () => {
  test("redirects a relative read into the acquired worktree", async () => {
    rememberWorktree("session-1", WORKTREE);
    const hook = createRedirectHook();
    const output = { args: { filePath: "src/a.rs" } };
    await hook["tool.execute.before"]({ tool: "read", sessionID: "session-1" }, output);
    expect(output.args.filePath).toBe(`${WORKTREE}/src/a.rs`);
  });

  test("redirects a bare bash cwd for a sub-agent session", async () => {
    rememberWorktree("parent-session", WORKTREE);
    const hook = createRedirectHook();
    const output = { args: { command: "pwd" } };
    await hook["tool.execute.before"]({ tool: "bash", sessionID: "child-session" }, output);
    expect(output.args.workdir).toBe(WORKTREE);
  });

  test("leaves absolute paths and no-worktree sessions unchanged", async () => {
    const hook = createRedirectHook();
    const noWorktree = { args: { filePath: "src/a.rs" } };
    await hook["tool.execute.before"]({ tool: "read", sessionID: "session-1" }, noWorktree);
    expect(noWorktree.args.filePath).toBe("src/a.rs");

    rememberWorktree("session-1", WORKTREE);
    const absolute = { args: { filePath: "/etc/hosts", workdir: "/tmp" } };
    await hook["tool.execute.before"]({ tool: "read", sessionID: "session-1" }, absolute);
    expect(absolute.args.filePath).toBe("/etc/hosts");
  });

  test("ignores calls without args", async () => {
    rememberWorktree("session-1", WORKTREE);
    const hook = createRedirectHook();
    await hook["tool.execute.before"]({ tool: "bash", sessionID: "session-1" }, {});
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
  test("injects --session into bash phasegent issue create", () => {
    const out = redirectArgs(
      "bash",
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
      "bash",
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

  test("leaves non-phasegent bash commands alone", () => {
    const out = redirectArgs("bash", WORKTREE, { command: "ls" }, "s1");
    expect(out.command).toBe("ls");
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

describe("tool.execute.before session injection without a worktree", () => {
  test("injects --session even when the registry is empty", async () => {
    const hook = createRedirectHook();
    const output = { args: { command: "phasegent issue create --title t" } };
    await hook["tool.execute.before"](
      { tool: "bash", sessionID: "fresh-session" },
      output,
    );
    expect(output.args.command).toBe(
      "phasegent issue create --title t --session fresh-session",
    );
    // No worktree was discovered (no git binding in this cwd): no workdir fill.
    expect(output.args.workdir).toBeUndefined();
  });
});
