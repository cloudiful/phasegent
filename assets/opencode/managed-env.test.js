// Managed shell bridge unit tests (issue #859 P1; issue #22 P1 verified
// identity).
//
// Pure or dependency-injected: no OpenCode, no phasegent CLI, no network.
// The bridge is per-setup (`createManagedShellBridge`), keyed by opaque
// nonce, never FIFO. Tests use injected `now`/`randomUUID` for determinism
// and assert exact byte restoration plus fresh-env-only mutation.
//
// The bridge never writes a role into the command text or the environment:
// it hands the shell the loopback broker endpoint and the opaque credential
// of the live tool call, and the CLI asks the broker for the role.

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
import { BROKER_ADDR_ENV, BROKER_TOKEN_ENV, createIdentityBroker } from "./src/broker.js";

// A deterministic stand-in for the identity broker: it records what the
// bridge asked for and hands back an opaque token.
function stubBroker(address = "127.0.0.1:9999") {
  const entries = new Map();
  let counter = 0;
  return {
    mint(role, sessionID, toolCallId) {
      counter += 1;
      const token = `token-${String(counter).padStart(4, "0")}-000000000000000000000000`;
      entries.set(token, { role, sessionID, toolCallId });
      return token;
    },
    address: () => address,
    revokeToolCall(toolCallId) {
      let removed = 0;
      for (const [token, entry] of entries) {
        if (entry.toolCallId === toolCallId) {
          entries.delete(token);
          removed += 1;
        }
      }
      return removed;
    },
    _entries: entries,
  };
}

function deterministicBridge(overrides = {}) {
  let counter = 0;
  let clock = overrides.start ?? 1_000_000;
  const broker = overrides.broker ?? stubBroker();
  const bridge = createManagedShellBridge({
    now: () => clock,
    randomUUID: () => {
      counter += 1;
      return `00000000-0000-4000-8000-${String(counter).padStart(12, "0")}`;
    },
    capacity: overrides.capacity,
    ttlMs: overrides.ttlMs,
    broker,
  });
  return {
    bridge,
    broker,
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
  test("known role restores exact bytes and adopts the broker env", async () => {
    const { bridge, broker } = deterministicBridge();
    const rewritten = "phasegent record list 1";
    const decorated = bridge.decorate(rewritten, "ses-1", "explore", "tool-1");
    expect(decorated.startsWith(MANAGED_CONTEXT_PREFIX)).toBe(true);
    expect(decorated.endsWith(rewritten)).toBe(true);

    const shellEvent = { command: decorated, env: { TERM: "x" } };
    const hook = createShellEnvHook(bridge);
    await hook(shellEvent);
    expect(shellEvent.command).toBe(rewritten);
    expect(shellEvent.env[BROKER_ADDR_ENV]).toBe("127.0.0.1:9999");
    expect(typeof shellEvent.env[BROKER_TOKEN_ENV]).toBe("string");
    expect(shellEvent.env.PHASEGENT_SESSION_ID).toBe("ses-1");
    expect(shellEvent.env.TERM).toBe("x");
    // The role itself never reaches the environment.
    expect("PHASEGENT_ROLE" in shellEvent.env).toBe(false);
    expect(bridge.size()).toBe(0);
    // The broker holds the host fact the CLI later asks for.
    const facts = [...broker._entries.values()];
    expect(facts.length).toBe(1);
    expect(facts[0].role).toBe("explore");
    expect(facts[0].sessionID).toBe("ses-1");
  });

  test("an unknown agent still gets a credential the broker refuses", async () => {
    const { bridge, broker } = deterministicBridge();
    const rewritten = "phasegent issue get 1";
    const decorated = bridge.decorate(rewritten, "ses-9", null, "tool-9");
    const shellEvent = {
      command: decorated,
      env: { PHASEGENT_ROLE: "executor", OTHER: "keep" },
    };
    await createShellEnvHook(bridge)(shellEvent);
    expect(shellEvent.command).toBe(rewritten);
    // The inherited role is dropped: the CLI never trusts it.
    expect("PHASEGENT_ROLE" in shellEvent.env).toBe(false);
    expect(shellEvent.env.PHASEGENT_SESSION_ID).toBe("ses-9");
    expect(shellEvent.env.OTHER).toBe("keep");
    // The broker stores no role, so it refuses the verification.
    const facts = [...broker._entries.values()];
    expect(facts.length).toBe(1);
    expect(facts[0].role).toBeNull();
  });

  test("a broker that refuses to mint still marks the shell as managed without a role", async () => {
    const broker = {
      mint: () => null,
      address: () => "127.0.0.1:9999",
      revokeToolCall: () => 0,
    };
    const { bridge } = deterministicBridge({ broker });
    const decorated = bridge.decorate("phasegent issue get 1", "ses-full", "executor", "tool-full");
    expect(decorated.startsWith(MANAGED_CONTEXT_PREFIX)).toBe(true);
    const shellEvent = {
      command: decorated,
      env: { [BROKER_ADDR_ENV]: "127.0.0.1:1", [BROKER_TOKEN_ENV]: "stale" },
    };
    await createShellEnvHook(bridge)(shellEvent);
    expect(shellEvent.command).toBe("phasegent issue get 1");
    expect(BROKER_ADDR_ENV in shellEvent.env).toBe(false);
    expect(BROKER_TOKEN_ENV in shellEvent.env).toBe(false);
    expect(shellEvent.env.PHASEGENT_SESSION_ID).toBe("ses-full");
  });

  test("a broker that throws while minting never becomes an ordinary operator", async () => {
    const broker = {
      mint: () => {
        throw new Error("mint failed");
      },
      address: () => "127.0.0.1:9999",
      revokeToolCall: () => 0,
    };
    const { bridge } = deterministicBridge({ broker });
    const decorated = bridge.decorate("phasegent issue get 1", "ses-throw", "executor", "tool-throw");
    expect(decorated.startsWith(MANAGED_CONTEXT_PREFIX)).toBe(true);
    const shellEvent = { command: decorated, env: {} };
    await createShellEnvHook(bridge)(shellEvent);
    expect(shellEvent.command).toBe("phasegent issue get 1");
    expect(BROKER_ADDR_ENV in shellEvent.env).toBe(false);
    expect(BROKER_TOKEN_ENV in shellEvent.env).toBe(false);
    expect(shellEvent.env.PHASEGENT_SESSION_ID).toBe("ses-throw");
  });

  test("a missing or blank lifecycle id stays managed without a credential", async () => {
    const broker = createIdentityBroker({ randomToken: () => "a".repeat(64) });
    const { bridge } = deterministicBridge({ broker });
    try {
      for (const id of [null, undefined, "", "   "]) {
        const decorated = bridge.decorate("phasegent issue get 1", "ses-noid", "reviewer", id);
        expect(decorated.startsWith(MANAGED_CONTEXT_PREFIX)).toBe(true);
        const shellEvent = { command: decorated, env: {} };
        await createShellEnvHook(bridge)(shellEvent);
        expect(shellEvent.command).toBe("phasegent issue get 1");
        expect(shellEvent.env.PHASEGENT_SESSION_ID).toBe("ses-noid");
        expect(BROKER_ADDR_ENV in shellEvent.env).toBe(false);
        expect(BROKER_TOKEN_ENV in shellEvent.env).toBe(false);
        expect(bridge.cleanupByToolCallId(id)).toBe(0);
      }
      expect(broker.size()).toBe(0);
    } finally {
      broker.stop();
    }
  });

  test("a stale inherited broker endpoint is never reused", async () => {
    const { bridge } = deterministicBridge();
    const decorated = bridge.decorate("phasegent issue get 1", "ses-1", "explore", "t1");
    const shellEvent = {
      command: decorated,
      env: { [BROKER_ADDR_ENV]: "127.0.0.1:1", [BROKER_TOKEN_ENV]: "stale" },
    };
    await createShellEnvHook(bridge)(shellEvent);
    expect(shellEvent.env[BROKER_ADDR_ENV]).toBe("127.0.0.1:9999");
    expect(shellEvent.env[BROKER_TOKEN_ENV]).not.toBe("stale");
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

  test("concurrent identical scripts keep the correct credential when consumed out of order", async () => {
    const { bridge } = deterministicBridge();
    const rewritten = "phasegent record list 1";
    // Two different sessions run the byte-identical rewritten script.
    const first = bridge.decorate(rewritten, "ses-A", "explore", "tool-A");
    const second = bridge.decorate(rewritten, "ses-B", "executor", "tool-B");
    expect(first).not.toBe(second);

    const hook = createShellEnvHook(bridge);
    // Consume out of order: second shell runs first.
    const secondEvent = { command: second, env: {} };
    await hook(secondEvent);
    const secondToken = secondEvent.env[BROKER_TOKEN_ENV];
    expect(typeof secondToken).toBe("string");
    expect(secondEvent.env.PHASEGENT_SESSION_ID).toBe("ses-B");

    const firstEvent = { command: first, env: {} };
    await hook(firstEvent);
    expect(firstEvent.env[BROKER_TOKEN_ENV]).not.toBe(secondToken);
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
    expect(shellEvent.env[BROKER_TOKEN_ENV]).toBeUndefined();
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
    expect(process.env[BROKER_TOKEN_ENV]).toBeUndefined();
  });

  test("missing env object is created without throwing", async () => {
    const { bridge } = deterministicBridge();
    const decorated = bridge.decorate("phasegent issue get 1", "ses-1", "explore", "t1");
    const shellEvent = { command: decorated };
    await createShellEnvHook(bridge)(shellEvent);
    expect(shellEvent.command).toBe("phasegent issue get 1");
    expect(shellEvent.env[BROKER_ADDR_ENV]).toBe("127.0.0.1:9999");
  });

  test("after-hook evicts by tool call id and revokes the broker credential", async () => {
    const { bridge, broker } = deterministicBridge();
    bridge.decorate("cmd-1", "ses-1", "explore", "tool-X");
    bridge.decorate("cmd-2", "ses-2", "explore", "tool-Y");
    expect(bridge.size()).toBe(2);
    expect(broker._entries.size).toBe(2);
    const after = createToolAfterHook(bridge);
    await after({ id: "tool-X" });
    expect(bridge.size()).toBe(1);
    expect(broker._entries.size).toBe(1);
    await after({ id: "missing" });
    expect(bridge.size()).toBe(1);
    expect(broker._entries.size).toBe(1);
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
