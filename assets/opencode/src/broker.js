// Loopback identity broker (issue #22 P1).
//
// The plugin owns the host facts of a managed shell: the agent id the host
// reported for the current tool call and the session id it runs in. The
// broker keeps those facts per live tool call and answers the CLI's
// verification request locally.
//
// Protocol: versioned newline-delimited JSON over one TCP connection per
// CLI invocation.
//
//   -> {"v":1,"op":"identify","token":"<opaque>","session":"<session id>"}
//   <- {"v":1,"ok":true,"role":"executor","session":"<session id>"}
//   <- {"v":1,"ok":false,"error":"unknown token"}
//
// The request never carries a role: the broker derives the role from the
// facts it stored, so a caller can never select an identity. The token is a
// strong random opaque value held in plugin memory; it is bound to one tool
// call, it may serve several CLI invocations inside that call, and it is
// revoked when the call completes or its entry expires.
//
// The listener binds the loopback interface only, so no verification
// request can leave the machine. Nothing about a token, a session id, or a
// role is ever logged.

import net from "node:net";
import { randomBytes } from "node:crypto";

export const BROKER_ADDR_ENV = "PHASEGENT_BROKER_ADDR";
export const BROKER_TOKEN_ENV = "PHASEGENT_BROKER_TOKEN";
export const BROKER_PROTOCOL_VERSION = 1;
export const BROKER_DEFAULT_TTL_MS = 5 * 60 * 1000;
export const BROKER_MAX_REQUEST_BYTES = 4096;
export const BROKER_MAX_ENTRIES = 128;
export const BROKER_MAX_CONNECTIONS = 64;
export const BROKER_REQUEST_DEADLINE_MS = 2000;

const TOKEN_PATTERN = /^[A-Za-z0-9]{32,128}$/;

function defaultNow() {
  return Date.now();
}

// A credential is 32 cryptographic random bytes rendered as hex. Where the
// runtime exposes no cryptographic source the generator fails closed: the
// broker never falls back to Math.random or any other weak entropy.
function defaultRandomToken() {
  try {
    return randomBytes(32).toString("hex");
  } catch (_) {
    return null;
  }
}

function optionNumber(options, name, fallback, minimum) {
  const value = options && options[name];
  return typeof value === "number" && Number.isFinite(value) && value >= minimum
    ? Math.floor(value)
    : fallback;
}

function isNonEmptyString(value) {
  return typeof value === "string" && value.length > 0;
}

// Immediate teardown for rejected, timed-out, and stopped connections. A
// reset (when the runtime exposes it) makes the peer observe closure promptly;
// plain destroy is the fallback. Never used to answer a valid request.
function destroySocket(socket) {
  try {
    if (typeof socket.resetAndDestroy === "function") {
      socket.resetAndDestroy();
      return;
    }
  } catch (_) {}
  try {
    socket.destroy();
  } catch (_) {}
}

// A host tool-call id is the only handle that can revoke this credential when
// the call completes. A missing, null, or blank id cannot be revoked, so it
// must never receive a token.
function isUsableLifecycleId(value) {
  return typeof value === "string" && value.trim().length > 0;
}

// Per-plugin-instance broker. Options are injectable for deterministic
// tests: `{ now, randomToken, ttlMs, maxEntries, maxConnections,
// requestDeadlineMs }`. Each setup call owns an isolated token map and its
// own loopback listener; no module-global state is shared across plugin
// instances.
export function createIdentityBroker(options) {
  const now =
    options && typeof options.now === "function" ? options.now : defaultNow;
  const randomToken =
    options && typeof options.randomToken === "function"
      ? options.randomToken
      : defaultRandomToken;
  const ttlMs =
    options && typeof options.ttlMs === "number" && options.ttlMs >= 0
      ? options.ttlMs
      : BROKER_DEFAULT_TTL_MS;
  const maxEntries = optionNumber(options, "maxEntries", BROKER_MAX_ENTRIES, 1);
  const maxConnections = optionNumber(
    options,
    "maxConnections",
    BROKER_MAX_CONNECTIONS,
    1,
  );
  const requestDeadlineMs = optionNumber(
    options,
    "requestDeadlineMs",
    BROKER_REQUEST_DEADLINE_MS,
    1,
  );

  // token -> { token, role, sessionID, toolCallId, timestamp }
  const entries = new Map();
  // socket -> { responded, buffer, deadline }
  const connections = new Map();
  let server = null;
  let address = null;

  function pruneExpired(current) {
    for (const [token, entry] of entries) {
      if (current - entry.timestamp > ttlMs) {
        entries.delete(token);
      }
    }
  }

  // Store one tool call and return its opaque token. A token is never
  // reused: a generated value that is already live is discarded and a new
  // one is drawn. Expired entries are pruned first, but a full live map
  // refuses the new credential instead of evicting another live call.
  function mint(role, sessionID, toolCallId) {
    if (!isNonEmptyString(sessionID)) return null;
    // No usable lifecycle handle means no later revocation: fail closed and
    // let the shell stay managed-without-role.
    if (!isUsableLifecycleId(toolCallId)) return null;
    const current = now();
    pruneExpired(current);
    if (entries.size >= maxEntries) return null;
    for (let attempt = 0; attempt < 8; attempt += 1) {
      const candidate = randomToken();
      if (typeof candidate !== "string" || !TOKEN_PATTERN.test(candidate)) {
        continue;
      }
      if (entries.has(candidate)) continue;
      entries.set(candidate, {
        token: candidate,
        // The role is the host fact the plugin observed. An unknown agent
        // has no role at all, and the broker then refuses the request
        // instead of guessing one.
        role: isNonEmptyString(role) ? role : null,
        sessionID,
        toolCallId,
        timestamp: current,
      });
      return candidate;
    }
    return null;
  }

  // Revoke every credential of one tool call: the call is over, so no
  // later invocation may reuse its identity.
  function revokeToolCall(toolCallId) {
    if (!isNonEmptyString(toolCallId)) return 0;
    let removed = 0;
    for (const [token, entry] of entries) {
      if (entry.toolCallId === toolCallId) {
        entries.delete(token);
        removed += 1;
      }
    }
    return removed;
  }

  // Answer one verification request. The request carries a token and a
  // session id; the reply echoes the session id of the stored fact, so a
  // reply for another session can never be mistaken for this one.
  function handle(line) {
    let request = null;
    try {
      request = JSON.parse(line);
    } catch (_) {
      return JSON.stringify({ v: BROKER_PROTOCOL_VERSION, ok: false, error: "malformed request" });
    }
    if (!request || typeof request !== "object") {
      return JSON.stringify({ v: BROKER_PROTOCOL_VERSION, ok: false, error: "malformed request" });
    }
    if (request.v !== BROKER_PROTOCOL_VERSION) {
      return JSON.stringify({
        v: BROKER_PROTOCOL_VERSION,
        ok: false,
        error: "unsupported protocol version",
      });
    }
    if (request.op !== "identify") {
      return JSON.stringify({ v: BROKER_PROTOCOL_VERSION, ok: false, error: "unsupported operation" });
    }
    if (!isNonEmptyString(request.token) || !isNonEmptyString(request.session)) {
      return JSON.stringify({ v: BROKER_PROTOCOL_VERSION, ok: false, error: "malformed request" });
    }
    const current = now();
    pruneExpired(current);
    const entry = entries.get(request.token);
    if (!entry) {
      return JSON.stringify({ v: BROKER_PROTOCOL_VERSION, ok: false, error: "unknown token" });
    }
    if (entry.sessionID !== request.session) {
      return JSON.stringify({ v: BROKER_PROTOCOL_VERSION, ok: false, error: "session mismatch" });
    }
    if (!entry.role) {
      return JSON.stringify({ v: BROKER_PROTOCOL_VERSION, ok: false, error: "unknown agent" });
    }
    return JSON.stringify({
      v: BROKER_PROTOCOL_VERSION,
      ok: true,
      role: entry.role,
      session: entry.sessionID,
    });
  }

  // Serve one connection: bound the concurrent sockets, read one request
  // line, answer at most once, and close. The absolute deadline starts at
  // accept and is never extended by incoming bytes, so a slow or trickling
  // client cannot hold a socket open past it.
  function serve(socket) {
    socket.on("error", () => {});
    if (connections.size >= maxConnections) {
      // Refuse transport outright: no overload reply, no tracked deadline,
      // and no untracked delayed callback. Immediate teardown releases the
      // descriptor, so a half-open rejected peer cannot survive stop.
      destroySocket(socket);
      return;
    }
    const state = { responded: false, buffer: Buffer.alloc(0), deadline: null };
    connections.set(socket, state);
    const clearDeadline = () => {
      if (state.deadline) {
        clearTimeout(state.deadline);
        state.deadline = null;
      }
    };
    const finish = () => {
      clearDeadline();
      connections.delete(socket);
    };
    const respond = (line) => {
      if (state.responded) return;
      state.responded = true;
      // Queue the reply but keep the absolute deadline running: a completed
      // request whose peer keeps its write half open still owns a socket
      // slot until the socket actually closes.
      try {
        socket.end(`${line}\n`);
      } catch (_) {}
    };
    state.deadline = setTimeout(() => {
      state.deadline = null;
      // The deadline bounds the whole socket lifetime, not only the request
      // read. Tear down on timeout whether or not a response was already
      // queued so a half-open client cannot retain a connection slot.
      state.responded = true;
      destroySocket(socket);
    }, requestDeadlineMs);
    if (state.deadline && typeof state.deadline.unref === "function") {
      state.deadline.unref();
    }
    socket.on("close", finish);
    socket.on("data", (chunk) => {
      if (state.responded) return;
      const bytes = Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk);
      // The cap counts actual bytes, not Unicode code units, and is checked
      // before concatenation so the buffer itself stays bounded.
      if (state.buffer.length + bytes.length > BROKER_MAX_REQUEST_BYTES) {
        respond(
          JSON.stringify({ v: BROKER_PROTOCOL_VERSION, ok: false, error: "request too large" }),
        );
        return;
      }
      state.buffer = Buffer.concat([state.buffer, bytes]);
      const newline = state.buffer.indexOf(0x0a);
      if (newline < 0) return;
      const line = state.buffer.subarray(0, newline).toString("utf8");
      let reply;
      try {
        reply = handle(line);
      } catch (_) {
        reply = JSON.stringify({ v: BROKER_PROTOCOL_VERSION, ok: false, error: "malformed request" });
      }
      respond(reply);
    });
  }

  // Bind the loopback interface on an ephemeral port. The address is
  // `127.0.0.1:<port>` and never leaves the machine. A listener that cannot
  // bind fails closed: `start` resolves `null` with no endpoint, so the
  // bridge still marks a managed shell with the session and no credential and
  // the CLI refuses the role instead of becoming an ordinary operator CLI.
  async function start() {
    if (address) return address;
    return new Promise((resolve) => {
      const listener = net.createServer(serve);
      let settled = false;
      const fail = () => {
        if (settled) return;
        settled = true;
        try {
          listener.close();
        } catch (_) {}
        server = null;
        address = null;
        resolve(null);
      };
      listener.on("error", fail);
      listener.listen(0, "127.0.0.1", () => {
        if (settled) return;
        const bound = listener.address();
        if (!bound || typeof bound === "string") {
          fail();
          return;
        }
        settled = true;
        server = listener;
        address = `127.0.0.1:${bound.port}`;
        resolve(address);
      });
    });
  }

  function stop() {
    for (const [socket, state] of connections) {
      if (state.deadline) {
        clearTimeout(state.deadline);
        state.deadline = null;
      }
      destroySocket(socket);
    }
    connections.clear();
    if (server) {
      try {
        server.close();
      } catch (_) {
        // closing a broker must never break the host
      }
      server = null;
    }
    address = null;
    entries.clear();
  }

  function size() {
    pruneExpired(now());
    return entries.size;
  }

  function connectionCount() {
    return connections.size;
  }

  return {
    mint,
    revokeToolCall,
    handle,
    start,
    stop,
    size,
    connectionCount,
    address: () => address,
    _entries: entries,
    _connections: connections,
    _ttlMs: ttlMs,
    _maxEntries: maxEntries,
    _maxConnections: maxConnections,
    _requestDeadlineMs: requestDeadlineMs,
  };
}
