// Identity broker credential-state unit tests (issue #22 P1).
//
// These exercise the broker's in-memory lifecycle directly: minting, refusal,
// capacity, expiry, revocation and request parsing over `handle`. The loopback
// listener and its wire protocol are user-validated integration acceptance now.

import { describe, expect, test } from "bun:test";
import {
  BROKER_MAX_ENTRIES,
  BROKER_PROTOCOL_VERSION,
  createIdentityBroker,
} from "./src/broker.js";

describe("identity broker resource bounds", () => {
  const deterministicToken = (() => {
    let counter = 0;
    return () => {
      counter += 1;
      return counter.toString(16).padStart(64, "0");
    };
  })();

  test("default credentials use 32 cryptographic bytes (64 hex characters)", () => {
    const broker = createIdentityBroker();
    try {
      const token = broker.mint("executor", "ses-crypto", "tool-crypto");
      expect(token).toMatch(/^[0-9a-f]{64}$/);
    } finally {
      broker.stop();
    }
  });

  test("a token source without cryptographic bytes fails closed", () => {
    const broker = createIdentityBroker({ randomToken: () => null });
    try {
      expect(broker.mint("executor", "ses-weak", "tool-weak")).toBeNull();
      expect(broker.size()).toBe(0);
    } finally {
      broker.stop();
    }
  });

  test("a full live map refuses a new credential without evicting live calls", () => {
    const broker = createIdentityBroker({ maxEntries: 2, randomToken: deterministicToken });
    try {
      const first = broker.mint("executor", "ses-a", "tool-a");
      const second = broker.mint("reviewer", "ses-b", "tool-b");
      const refused = broker.mint("orchestrator", "ses-c", "tool-c");
      expect(refused).toBeNull();
      expect(broker.size()).toBe(2);
      expect(broker.handle(JSON.stringify({
        v: BROKER_PROTOCOL_VERSION,
        op: "identify",
        token: first,
        session: "ses-a",
      }))).toContain('"ok":true');
      expect(broker.handle(JSON.stringify({
        v: BROKER_PROTOCOL_VERSION,
        op: "identify",
        token: second,
        session: "ses-b",
      }))).toContain('"ok":true');
    } finally {
      broker.stop();
    }
  });

  test("the default credential capacity is finite", () => {
    const broker = createIdentityBroker({ randomToken: deterministicToken });
    try {
      for (let index = 0; index <= BROKER_MAX_ENTRIES; index += 1) {
        const token = broker.mint("executor", "ses-full", `tool-${index}`);
        if (index < BROKER_MAX_ENTRIES) expect(token).not.toBeNull();
        else expect(token).toBeNull();
      }
      expect(broker.size()).toBe(BROKER_MAX_ENTRIES);
    } finally {
      broker.stop();
    }
  });

  test("expiry frees capacity", () => {
    let clock = 1_000;
    const broker = createIdentityBroker({
      now: () => clock,
      ttlMs: 1000,
      maxEntries: 1,
      randomToken: deterministicToken,
    });
    try {
      expect(broker.mint("executor", "ses-old", "tool-old")).not.toBeNull();
      expect(broker.mint("executor", "ses-new", "tool-new")).toBeNull();
      clock += 1001;
      expect(broker.mint("executor", "ses-new", "tool-new")).not.toBeNull();
      expect(broker.size()).toBe(1);
    } finally {
      broker.stop();
    }
  });

  test("revocation frees capacity", () => {
    const broker = createIdentityBroker({ maxEntries: 1, randomToken: deterministicToken });
    try {
      expect(broker.mint("executor", "ses-r", "tool-r")).not.toBeNull();
      expect(broker.mint("executor", "ses-r2", "tool-r2")).toBeNull();
      expect(broker.revokeToolCall("tool-r")).toBe(1);
      expect(broker.mint("executor", "ses-r2", "tool-r2")).not.toBeNull();
    } finally {
      broker.stop();
    }
  });

  test("a missing or blank lifecycle id receives no credential", () => {
    const broker = createIdentityBroker({ randomToken: deterministicToken });
    try {
      for (const id of [null, undefined, "", "   "]) {
        expect(broker.mint("reviewer", "ses-noid", id)).toBeNull();
      }
      expect(broker.size()).toBe(0);
    } finally {
      broker.stop();
    }
  });
});
