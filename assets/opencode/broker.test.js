// Identity broker unit tests (issue #22 P1).
//
// The broker owns the host facts of a managed tool call and answers the
// CLI's verification request over a loopback TCP connection. These tests
// use a real socket against a real listener, so the wire protocol, the
// token lifecycle, and the revocation are exercised for real.

import { describe, expect, test } from "bun:test";
import { connect } from "node:net";
import {
  BROKER_ADDR_ENV,
  BROKER_MAX_ENTRIES,
  BROKER_PROTOCOL_VERSION,
  BROKER_TOKEN_ENV,
  createIdentityBroker,
} from "./src/broker.js";

function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

// One request/response round trip over a fresh loopback connection.
function identify(address, request) {
  return new Promise((resolve, reject) => {
    const [host, port] = address.split(":");
    const socket = connect({ host, port: Number(port) }, () => {
      socket.write(`${JSON.stringify(request)}\n`);
    });
    let buffer = "";
    socket.on("data", (chunk) => {
      buffer += chunk.toString("utf8");
    });
    socket.on("error", reject);
    socket.on("close", () => {
      const line = buffer.split("\n")[0];
      try {
        resolve(JSON.parse(line));
      } catch (error) {
        reject(error);
      }
    });
  });
}

function openSocket(address) {
  return new Promise((resolve, reject) => {
    const [host, port] = address.split(":");
    const socket = connect({ host, port: Number(port) }, () => resolve(socket));
    socket.on("error", reject);
  });
}

function openHalfOpenSocket(address) {
  return new Promise((resolve, reject) => {
    const [host, port] = address.split(":");
    const socket = connect(
      { host, port: Number(port), allowHalfOpen: true },
      () => resolve(socket),
    );
    socket.on("error", reject);
  });
}

function waitForClose(socket, timeoutMs = 1000) {
  return new Promise((resolve) => {
    if (socket.destroyed) {
      resolve(true);
      return;
    }
    const timer = setTimeout(() => resolve(false), timeoutMs);
    if (typeof timer.unref === "function") timer.unref();
    socket.on("error", () => {});
    socket.once("close", () => {
      clearTimeout(timer);
      resolve(true);
    });
  });
}

function collect(socket, timeoutMs = 1000) {
  return new Promise((resolve) => {
    const chunks = [];
    socket.on("data", (chunk) => chunks.push(chunk));
    socket.on("error", () => {});
    const timer = setTimeout(() => resolve(Buffer.concat(chunks).toString("utf8")), timeoutMs);
    if (typeof timer.unref === "function") timer.unref();
    socket.on("close", () => {
      clearTimeout(timer);
      resolve(Buffer.concat(chunks).toString("utf8"));
    });
  });
}

describe("identity broker", () => {
  test("a minted token verifies the stored role for its own session", async () => {
    const broker = createIdentityBroker();
    const address = await broker.start();
    try {
      const token = broker.mint("executor", "ses-1", "tool-1");
      expect(typeof token).toBe("string");
      expect(token.length).toBeGreaterThanOrEqual(32);
      const reply = await identify(address, {
        v: BROKER_PROTOCOL_VERSION,
        op: "identify",
        token,
        session: "ses-1",
      });
      expect(reply.ok).toBe(true);
      expect(reply.role).toBe("executor");
      expect(reply.session).toBe("ses-1");
    } finally {
      broker.stop();
    }
  });

  test("the same token serves several invocations of one live tool call", async () => {
    const broker = createIdentityBroker();
    const address = await broker.start();
    try {
      const token = broker.mint("reviewer", "ses-2", "tool-2");
      for (let attempt = 0; attempt < 3; attempt += 1) {
        const reply = await identify(address, {
          v: BROKER_PROTOCOL_VERSION,
          op: "identify",
          token,
          session: "ses-2",
        });
        expect(reply.ok).toBe(true);
        expect(reply.role).toBe("reviewer");
      }
    } finally {
      broker.stop();
    }
  });

  test("an unknown token is refused", async () => {
    const broker = createIdentityBroker();
    const address = await broker.start();
    try {
      const reply = await identify(address, {
        v: BROKER_PROTOCOL_VERSION,
        op: "identify",
        token: "0".repeat(64),
        session: "ses-3",
      });
      expect(reply.ok).toBe(false);
      expect(reply.error).toBe("unknown token");
    } finally {
      broker.stop();
    }
  });

  test("a token of another session is refused", async () => {
    const broker = createIdentityBroker();
    const address = await broker.start();
    try {
      const token = broker.mint("executor", "ses-4", "tool-4");
      const reply = await identify(address, {
        v: BROKER_PROTOCOL_VERSION,
        op: "identify",
        token,
        session: "ses-other",
      });
      expect(reply.ok).toBe(false);
      expect(reply.error).toBe("session mismatch");
    } finally {
      broker.stop();
    }
  });

  test("an unknown agent never maps to a role", async () => {
    const broker = createIdentityBroker();
    const address = await broker.start();
    try {
      const token = broker.mint(null, "ses-5", "tool-5");
      const reply = await identify(address, {
        v: BROKER_PROTOCOL_VERSION,
        op: "identify",
        token,
        session: "ses-5",
      });
      expect(reply.ok).toBe(false);
      expect(reply.error).toBe("unknown agent");
    } finally {
      broker.stop();
    }
  });

  test("completing the tool call revokes its credential", async () => {
    const broker = createIdentityBroker();
    const address = await broker.start();
    try {
      const token = broker.mint("executor", "ses-6", "tool-6");
      expect(broker.revokeToolCall("tool-6")).toBe(1);
      const reply = await identify(address, {
        v: BROKER_PROTOCOL_VERSION,
        op: "identify",
        token,
        session: "ses-6",
      });
      expect(reply.ok).toBe(false);
      expect(reply.error).toBe("unknown token");
    } finally {
      broker.stop();
    }
  });

  test("an expired credential is refused", async () => {
    let clock = 1_000;
    const broker = createIdentityBroker({ now: () => clock, ttlMs: 1000 });
    const address = await broker.start();
    try {
      const token = broker.mint("executor", "ses-7", "tool-7");
      clock += 1001;
      const reply = await identify(address, {
        v: BROKER_PROTOCOL_VERSION,
        op: "identify",
        token,
        session: "ses-7",
      });
      expect(reply.ok).toBe(false);
      expect(reply.error).toBe("unknown token");
    } finally {
      broker.stop();
    }
  });

  test("the listener binds the loopback interface only", async () => {
    const broker = createIdentityBroker();
    const address = await broker.start();
    try {
      expect(address.startsWith("127.0.0.1:")).toBe(true);
    } finally {
      broker.stop();
    }
  });

  test("the env names are the documented ones", () => {
    expect(BROKER_ADDR_ENV).toBe("PHASEGENT_BROKER_ADDR");
    expect(BROKER_TOKEN_ENV).toBe("PHASEGENT_BROKER_TOKEN");
  });
});

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

  test("an idle connection closes within the absolute request deadline", async () => {
    const broker = createIdentityBroker({ requestDeadlineMs: 60 });
    const address = await broker.start();
    try {
      const socket = await openSocket(address);
      const started = Date.now();
      const closed = await new Promise((resolve) => {
        socket.on("close", () => resolve(true));
        setTimeout(() => resolve(false), 1000).unref?.();
      });
      expect(closed).toBe(true);
      expect(Date.now() - started).toBeLessThan(1000);
      expect(broker.connectionCount()).toBe(0);
    } finally {
      broker.stop();
    }
  });

  test("a trickling client cannot extend the absolute deadline", async () => {
    const broker = createIdentityBroker({ requestDeadlineMs: 80 });
    const address = await broker.start();
    try {
      const socket = await openSocket(address);
      let closedAt = null;
      socket.on("close", () => {
        closedAt = Date.now();
      });
      const started = Date.now();
      for (let index = 0; index < 12; index += 1) {
        if (!socket.destroyed) {
          try {
            socket.write("{");
          } catch (_) {}
        }
        await sleep(25);
      }
      expect(closedAt).not.toBeNull();
      expect(closedAt - started).toBeLessThan(300);
    } finally {
      broker.stop();
    }
  });

  test("a half-open client cannot retain its slot after the reply", async () => {
    const broker = createIdentityBroker({ requestDeadlineMs: 80, maxConnections: 1 });
    const address = await broker.start();
    try {
      const token = broker.mint("executor", "ses-half", "tool-half");
      const [host, port] = address.split(":");
      const socket = await new Promise((resolve, reject) => {
        const client = connect(
          { host, port: Number(port), allowHalfOpen: true },
          () => resolve(client),
        );
        client.on("error", reject);
      });
      let response = "";
      socket.on("data", (chunk) => {
        response += chunk.toString("utf8");
      });
      socket.write(`${JSON.stringify({
        v: BROKER_PROTOCOL_VERSION,
        op: "identify",
        token,
        session: "ses-half",
      })}\n`);
      const waitUntil = Date.now() + 1000;
      while (!response.includes("\n") && Date.now() < waitUntil) await sleep(5);
      expect(response).toContain('"ok":true');
      // The client keeps its write half open; the server must still destroy
      // the socket and free the only connection slot at the deadline.
      await sleep(200);
      expect(broker.connectionCount()).toBe(0);
      const reply = await identify(address, {
        v: BROKER_PROTOCOL_VERSION,
        op: "identify",
        token,
        session: "ses-half",
      });
      expect(reply.ok).toBe(true);
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

  test("an excess half-open connection is destroyed promptly and cannot survive stop", async () => {
    const broker = createIdentityBroker({ maxConnections: 1, requestDeadlineMs: 5000 });
    const address = await broker.start();
    try {
      const accepted = await openHalfOpenSocket(address);
      expect(broker.connectionCount()).toBe(1);
      for (let attempt = 0; attempt < 3; attempt += 1) {
        const excess = await openHalfOpenSocket(address);
        let reply = "";
        excess.on("data", (chunk) => {
          reply += chunk.toString("utf8");
        });
        expect(await waitForClose(excess, 1000)).toBe(true);
        expect(reply).toBe("");
        expect(broker.connectionCount()).toBe(1);
      }
      broker.stop();
      expect(await waitForClose(accepted, 1000)).toBe(true);
      expect(broker.connectionCount()).toBe(0);
    } finally {
      broker.stop();
    }
  });

  test("the request byte cap counts actual bytes and answers once", async () => {
    const broker = createIdentityBroker();
    const address = await broker.start();
    try {
      const oversized = await openSocket(address);
      // 1500 three-byte characters are 4500 bytes but only 1500 code units.
      oversized.write("\u20ac".repeat(1500));
      const oversizedText = await collect(oversized);
      expect(oversizedText).toContain("request too large");
      expect(oversizedText.split("\n").filter(Boolean)).toHaveLength(1);

      const double = await openSocket(address);
      const token = broker.mint("executor", "ses-once", "tool-once");
      const line = JSON.stringify({
        v: BROKER_PROTOCOL_VERSION,
        op: "identify",
        token,
        session: "ses-once",
      });
      double.write(`${line}\n${line}\n`);
      const doubleText = await collect(double);
      expect(doubleText.split("\n").filter(Boolean)).toHaveLength(1);
      expect(doubleText).toContain('"ok":true');
    } finally {
      broker.stop();
    }
  });

  test("stop destroys active sockets, clears timers and credentials", async () => {
    const broker = createIdentityBroker({ requestDeadlineMs: 60_000 });
    const address = await broker.start();
    const socket = await openSocket(address);
    expect(broker.connectionCount()).toBe(1);
    broker.mint("executor", "ses-stop", "tool-stop");
    broker.stop();
    const closed = await new Promise((resolve) => {
      socket.on("close", () => resolve(true));
      setTimeout(() => resolve(false), 1000).unref?.();
    });
    expect(closed).toBe(true);
    expect(broker.connectionCount()).toBe(0);
    expect(broker.size()).toBe(0);
  });
});
