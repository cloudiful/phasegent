// Managed shell bridge unit tests (issue #859 P1).
//
// Pure or dependency-injected: no OpenCode, no phasegent CLI, no network.
// The bridge is per-setup (`createManagedShellBridge`), keyed by opaque
// nonce, never FIFO. Tests use injected `now`/`randomUUID` for determinism
// and assert exact byte restoration plus fresh-env-only mutation.

import { describe, expect, test } from "bun:test";
import {
  MANAGED_BRIDGE_CAPACITY,
  MANAGED_BRIDGE_TTL_MS,
  MANAGED_CONTEXT_PREFIX,
  createManagedShellBridge,
  createShellEnvHook,
  createToolAfterHook,
  isShellHookSupported,
  managedHeaderFor,
  parseManagedNonce,
} from "./src/managed-env.js";

function deterministicBridge(overrides = {}) {
  let counter = 0;
  let clock = overrides.start ?? 1_000_000;
  const bridge = createManagedShellBridge({
    now: () => clock,
    randomUUID: () => {
      counter += 1;
      return `00000000-0000-4000-8000-${String(counter).padStart(12, "0")}`;
    },
    capacity: overrides.capacity,
    ttlMs: overrides.ttlMs,
  });
  return {
    bridge,
    advance(ms) {
      clock += ms;
    },
    now: () => clock,
  };
}

describe("managed header", () => {
  test("header is a single POSIX/PowerShell-safe comment line", () => {
    const header = managedHeaderFor("abc-123");
    expect(header).toBe("# phasegent-context:abc-123\n");
    expect(header.endsWith("\n")).toBe(true);
    expect(header.split("\n")).toHaveLength(3 - 1);
  });

  test("parseManagedNonce extracts only well-formed leading headers", () => {
    expect(parseManagedNonce("# phasegent-context:abc-123\nls")).toBe("abc-123");
    expect(parseManagedNonce("ls")).toBeNull();
    expect(parseManagedNonce("# other:abc\nls")).toBeNull();
    expect(parseManagedNonce("# phasegent-context:bad nonce!\nls")).toBeNull();
    expect(parseManagedNonce("# phasegent-context:abc")).toBeNull();
    expect(parseManagedNonce(undefined)).toBeNull();
  });
});

describe("isShellHookSupported", () => {
  test("requires a shell.hook function", () => {
    expect(isShellHookSupported({ shell: { hook: async () => {} } })).toBe(true);
    expect(isShellHookSupported({})).toBe(false);
    expect(isShellHookSupported({ shell: {} })).toBe(false);
    expect(isShellHookSupported(undefined)).toBe(false);
  });
});

describe("decorate and consume", () => {
  test("known role restores exact bytes and adopts env on the fresh object", async () => {
    const { bridge } = deterministicBridge();
    const rewritten = "PHASEGENT_ROLE=explore phasegent record list 1";
    const decorated = bridge.decorate(rewritten, "ses-1", "explore", "tool-1");
    expect(decorated.startsWith(MANAGED_CONTEXT_PREFIX)).toBe(true);
    expect(decorated.endsWith(rewritten)).toBe(true);

    const shellEvent = { command: decorated, env: { TERM: "x" } };
    const hook = createShellEnvHook(bridge);
    await hook(shellEvent);
    expect(shellEvent.command).toBe(rewritten);
    expect(shellEvent.env.PHASEGENT_ROLE).toBe("explore");
    expect(shellEvent.env.PHASEGENT_SESSION_ID).toBe("ses-1");
    expect(shellEvent.env.TERM).toBe("x");
    expect(bridge.size()).toBe(0);
  });

  test("unknown role deletes inherited role but still marks managed", async () => {
    const { bridge } = deterministicBridge();
    const rewritten = "phasegent issue get 1";
    const decorated = bridge.decorate(rewritten, "ses-9", null, "tool-9");
    const shellEvent = {
      command: decorated,
      env: { PHASEGENT_ROLE: "executor", OTHER: "keep" },
    };
    await createShellEnvHook(bridge)(shellEvent);
    expect(shellEvent.command).toBe(rewritten);
    expect("PHASEGENT_ROLE" in shellEvent.env).toBe(false);
    expect(shellEvent.env.PHASEGENT_SESSION_ID).toBe("ses-9");
    expect(shellEvent.env.OTHER).toBe("keep");
  });

  test("non-canonical role string is treated as unknown", async () => {
    const { bridge } = deterministicBridge();
    const decorated = bridge.decorate("phasegent issue get 1", "ses-1", "task-executor", "t1");
    const shellEvent = { command: decorated, env: { PHASEGENT_ROLE: "orchestrator" } };
    await createShellEnvHook(bridge)(shellEvent);
    expect("PHASEGENT_ROLE" in shellEvent.env).toBe(false);
    expect(shellEvent.env.PHASEGENT_SESSION_ID).toBe("ses-1");
  });

  test("missing session id decorates nothing", () => {
    const { bridge } = deterministicBridge();
    expect(bridge.decorate("phasegent issue get 1", "", "explore", "t1")).toBe(
      "phasegent issue get 1",
    );
    expect(bridge.decorate("phasegent issue get 1", null, "explore", "t1")).toBe(
      "phasegent issue get 1",
    );
    expect(bridge.size()).toBe(0);
  });

  test("concurrent identical scripts keep correct role when consumed out of order", async () => {
    const { bridge } = deterministicBridge();
    const rewritten = "PHASEGENT_ROLE=explore phasegent record list 1";
    // Two different sessions run the byte-identical rewritten script.
    const first = bridge.decorate(rewritten, "ses-A", "explore", "tool-A");
    const second = bridge.decorate(rewritten, "ses-B", "executor", "tool-B");
    expect(first).not.toBe(second);

    const hook = createShellEnvHook(bridge);
    // Consume out of order: second shell runs first.
    const secondEvent = { command: second, env: {} };
    await hook(secondEvent);
    expect(secondEvent.env.PHASEGENT_ROLE).toBe("executor");
    expect(secondEvent.env.PHASEGENT_SESSION_ID).toBe("ses-B");

    const firstEvent = { command: first, env: {} };
    await hook(firstEvent);
    expect(firstEvent.env.PHASEGENT_ROLE).toBe("explore");
    expect(firstEvent.env.PHASEGENT_SESSION_ID).toBe("ses-A");
  });

  test("forged nonce never provides identity and leaves env untouched", async () => {
    const { bridge } = deterministicBridge();
    const shellEvent = {
      command: "# phasegent-context:forged-999\nphasegent issue get 1",
      env: { KEEP: "1" },
    };
    await createShellEnvHook(bridge)(shellEvent);
    expect(shellEvent.command).toBe("# phasegent-context:forged-999\nphasegent issue get 1");
    expect(shellEvent.env.PHASEGENT_ROLE).toBeUndefined();
    expect(shellEvent.env.PHASEGENT_SESSION_ID).toBeUndefined();
    expect(shellEvent.env.KEEP).toBe("1");
  });

  test("mismatched script never pairs and is single-use", async () => {
    const { bridge } = deterministicBridge();
    const decorated = bridge.decorate("phasegent issue get 1", "ses-1", "explore", "t1");
    const nonce = parseManagedNonce(decorated);
    const tampered = `${managedHeaderFor(nonce)}phasegent issue get 2`;
    const shellEvent = { command: tampered, env: {} };
    await createShellEnvHook(bridge)(shellEvent);
    // Mismatch: command stays (header inert), no env identity.
    expect(shellEvent.command).toBe(tampered);
    expect(shellEvent.env.PHASEGENT_SESSION_ID).toBeUndefined();
    // Replay of the original now fails too: the entry was consumed once.
    const replay = { command: decorated, env: {} };
    await createShellEnvHook(bridge)(replay);
    expect(replay.env.PHASEGENT_SESSION_ID).toBeUndefined();
    expect(bridge.size()).toBe(0);
  });

  test("replayed header never authorizes twice", async () => {
    const { bridge } = deterministicBridge();
    const decorated = bridge.decorate("phasegent issue get 1", "ses-1", "explore", "t1");
    const first = { command: decorated, env: {} };
    await createShellEnvHook(bridge)(first);
    expect(first.env.PHASEGENT_SESSION_ID).toBe("ses-1");
    const second = { command: decorated, env: {} };
    await createShellEnvHook(bridge)(second);
    expect(second.env.PHASEGENT_SESSION_ID).toBeUndefined();
  });

  test("expired entry never provides identity", async () => {
    const ctx = deterministicBridge({ ttlMs: 1000 });
    const decorated = ctx.bridge.decorate("phasegent issue get 1", "ses-1", "explore", "t1");
    ctx.advance(1001);
    const shellEvent = { command: decorated, env: {} };
    await createShellEnvHook(ctx.bridge)(shellEvent);
    expect(shellEvent.env.PHASEGENT_SESSION_ID).toBeUndefined();
    expect(shellEvent.command).toBe(decorated);
  });

  test("overflow discards oldest without reuse or pairing", async () => {
    const ctx = deterministicBridge({ capacity: 2 });
    const first = ctx.bridge.decorate("cmd-1", "ses-1", "explore", "t1");
    ctx.bridge.decorate("cmd-2", "ses-2", "explore", "t2");
    // Third insert evicts the oldest (first).
    ctx.bridge.decorate("cmd-3", "ses-3", "explore", "t3");
    expect(ctx.bridge.size()).toBe(2);
    const evicted = { command: first, env: {} };
    await createShellEnvHook(ctx.bridge)(evicted);
    expect(evicted.env.PHASEGENT_SESSION_ID).toBeUndefined();
  });

  test("does not mutate global process.env", async () => {
    const { bridge } = deterministicBridge();
    const before = process.env.PHASEGENT_ROLE;
    const decorated = bridge.decorate("phasegent issue get 1", "ses-1", "explore", "t1");
    const shellEvent = { command: decorated, env: {} };
    await createShellEnvHook(bridge)(shellEvent);
    expect(process.env.PHASEGENT_ROLE).toBe(before);
    expect(process.env.PHASEGENT_SESSION_ID).not.toBe("ses-1");
  });

  test("missing env object is created without throwing", async () => {
    const { bridge } = deterministicBridge();
    const decorated = bridge.decorate("phasegent issue get 1", "ses-1", "explore", "t1");
    const shellEvent = { command: decorated };
    await createShellEnvHook(bridge)(shellEvent);
    expect(shellEvent.command).toBe("phasegent issue get 1");
    expect(shellEvent.env.PHASEGENT_ROLE).toBe("explore");
  });

  test("after-hook evicts by tool call id", async () => {
    const { bridge } = deterministicBridge();
    bridge.decorate("cmd-1", "ses-1", "explore", "tool-X");
    bridge.decorate("cmd-2", "ses-2", "explore", "tool-Y");
    expect(bridge.size()).toBe(2);
    const after = createToolAfterHook(bridge);
    await after({ id: "tool-X" });
    expect(bridge.size()).toBe(1);
    await after({ id: "missing" });
    expect(bridge.size()).toBe(1);
  });

  test("bridges are per-instance and never share nonces", () => {
    const first = deterministicBridge();
    const second = deterministicBridge();
    const a = first.bridge.decorate("cmd", "ses-1", "explore", "t1");
    // Same deterministic nonce in the second isolated bridge is a different
    // entry: consuming in the wrong bridge never pairs.
    const foreign = { command: a, env: {} };
    // `second.bridge` holds no entry for `a`'s nonce, so consume fails.
    const result = second.bridge.consume(foreign);
    expect(result.consumed).toBe(false);
    expect(foreign.env.PHASEGENT_SESSION_ID).toBeUndefined();
  });
});

describe("bridge bounds", () => {
  test("capacity and TTL match the approved limits", () => {
    expect(MANAGED_BRIDGE_CAPACITY).toBe(64);
    expect(MANAGED_BRIDGE_TTL_MS).toBe(5 * 60 * 1000);
  });
});
