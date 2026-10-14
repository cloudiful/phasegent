// Managed shell execution bridge (issue #859 P1).
//
// Per-plugin-instance correlation between `tool.execute.before` and
// `shell.create.before` via an opaque unique per-tool-call nonce, not a
// command FIFO and not command-only matching.
//
// Flow:
//   1. `tool.execute.before` first performs the existing phasegent command
//      rewrite (`rewritePhasegentCommand`), then the bridge stores
//      `{ nonce, sessionID, role, toolCallId, expected, timestamp }` where
//      `expected` is the exact header-prefixed command, and prepends one
//      POSIX/PowerShell-safe comment line `# phasegent-context:<nonce>\n`.
//      The header is inert in both shells and never an identity claim.
//   2. `shell.create.before` consumes only on exact nonce plus exact
//      decorated-command match, once. It strips only its own leading line,
//      restores the exact rewritten bytes, and mutates the fresh `event.env`
//      properties only: `PHASEGENT_ROLE` when the role is trusted, otherwise
//      it deletes any inherited `PHASEGENT_ROLE`, and always sets
//      `PHASEGENT_SESSION_ID` to the real tool session. A real session id is
//      required; without one the bridge decorates nothing.
//   3. `tool.execute.after` evicts by tool call id so cancelled or failed
//      calls never leak. Bounded TTL (5 minutes) and capacity (64 entries)
//      evict lazily; an evicted entry only deletes, never reuses or pairs.
//      A lost entry falls back to the host native `OPENCODE_SESSION_ID`
//      marker rejection, never a guessed identity.
//
// No global `process.env` mutation, no FIFO, no mutable cross-session current
// role. Expired, replayed, forged, or mismatched headers never provide an
// identity. No full commands or secret data are logged; warnings stay generic.

export const MANAGED_CONTEXT_PREFIX = "# phasegent-context:";
export const MANAGED_ROLE_ENV = "PHASEGENT_ROLE";
export const MANAGED_SESSION_ENV = "PHASEGENT_SESSION_ID";
export const MANAGED_BRIDGE_CAPACITY = 64;
export const MANAGED_BRIDGE_TTL_MS = 5 * 60 * 1000;

const NONCE_PATTERN = /^[A-Za-z0-9._:-]{1,128}$/;

function defaultNow() {
  return Date.now();
}

function defaultRandomUUID() {
  try {
    const cryptoObject =
      (typeof globalThis !== "undefined" && globalThis.crypto) || null;
    if (cryptoObject && typeof cryptoObject.randomUUID === "function") {
      return cryptoObject.randomUUID();
    }
  } catch (_) {
    // fall through to deterministic fallback below
  }
  // Fallback is only reached where `crypto.randomUUID` is unavailable; it is
  // never used to reuse a live nonce because every generated value is stored
  // and checked for collision before use.
  const random = Math.floor(Math.random() * 0xffffffff);
  const time = Date.now() & 0xffffffff;
  return `fallback-${time.toString(16)}-${random.toString(16)}`;
}

function isNonEmptyString(value) {
  return typeof value === "string" && value.length > 0;
}

// Capability probe: the host must expose `context.shell.hook` before the
// bridge decorates anything with an identity promise. A missing surface
// degrades to the existing rewrite-only path with an explicit warning.
export function isShellHookSupported(context) {
  return Boolean(
    context && context.shell && typeof context.shell.hook === "function",
  );
}

export function isToolAfterHookSupported(context) {
  return Boolean(
    context && context.tool && typeof context.tool.hook === "function",
  );
}

// Parse the leading `# phasegent-context:<nonce>\n` header. Returns the nonce
// string, or null when the command carries no well-formed bridge header.
export function parseManagedNonce(command) {
  if (typeof command !== "string") return null;
  if (!command.startsWith(MANAGED_CONTEXT_PREFIX)) return null;
  const newline = command.indexOf("\n");
  if (newline < 0) return null;
  const nonce = command.slice(MANAGED_CONTEXT_PREFIX.length, newline).trim();
  if (!NONCE_PATTERN.test(nonce)) return null;
  return nonce;
}

export function managedHeaderFor(nonce) {
  return `${MANAGED_CONTEXT_PREFIX}${nonce}\n`;
}

// Per-setup bridge factory. Options are injectable for deterministic tests:
// `{ now, randomUUID, capacity, ttlMs }`. Each setup call owns an isolated
// Map; no module-global state is shared across plugin instances.
export function createManagedShellBridge(options) {
  const now =
    options && typeof options.now === "function" ? options.now : defaultNow;
  const randomUUID =
    options && typeof options.randomUUID === "function"
      ? options.randomUUID
      : defaultRandomUUID;
  const capacity =
    options && typeof options.capacity === "number" && options.capacity > 0
      ? Math.floor(options.capacity)
      : MANAGED_BRIDGE_CAPACITY;
  const ttlMs =
    options && typeof options.ttlMs === "number" && options.ttlMs >= 0
      ? options.ttlMs
      : MANAGED_BRIDGE_TTL_MS;

  // nonce -> { nonce, sessionID, role, toolCallId, expected, timestamp }
  // Insertion order is the eviction order: the oldest entry is discarded
  // first when capacity is reached. Entries are never reused or re-paired.
  const entries = new Map();

  function pruneExpired(current) {
    for (const [nonce, entry] of entries) {
      if (current - entry.timestamp > ttlMs) {
        entries.delete(nonce);
      }
    }
  }

  function evictOldestIfNeeded() {
    while (entries.size >= capacity) {
      const oldest = entries.keys().next();
      if (oldest.done) break;
      entries.delete(oldest.value);
    }
  }

  function generateNonce() {
    // A stored nonce is never reused: on the vanishingly unlikely collision
    // the generator retries instead of overwriting the live entry.
    for (let attempt = 0; attempt < 8; attempt += 1) {
      const candidate = String(randomUUID());
      if (!NONCE_PATTERN.test(candidate)) continue;
      if (!entries.has(candidate)) return candidate;
    }
    return null;
  }

  // Store one tool call and return the header-prefixed command. Returns the
  // input unchanged when decoration is impossible (missing session id or
  // unusable nonce); the caller keeps the rewrite-only path in that case.
  function decorate(rewritten, sessionID, role, toolCallId) {
    if (typeof rewritten !== "string") return rewritten;
    if (!isNonEmptyString(sessionID)) return rewritten;
    const current = now();
    pruneExpired(current);
    const nonce = generateNonce();
    if (!nonce) return rewritten;
    evictOldestIfNeeded();
    const header = managedHeaderFor(nonce);
    const expected = `${header}${rewritten}`;
    const trustedRole =
      role === "orchestrator" ||
      role === "executor" ||
      role === "reviewer" ||
      role === "explore"
        ? role
        : null;
    entries.set(nonce, {
      nonce,
      sessionID,
      role: trustedRole,
      toolCallId: isNonEmptyString(toolCallId) ? toolCallId : null,
      expected,
      timestamp: current,
    });
    return expected;
  }

  // Consume one shell invocation. On exact nonce plus exact decorated-command
  // match with a live entry, strips the header, restores the exact rewritten
  // bytes, mutates only `event.env` properties, deletes the entry (single
  // use), and returns `{ consumed: true }`. Every other shape — missing,
  // forged, mismatched, expired, or replayed — leaves `event.command` and
  // `event.env` untouched and returns `{ consumed: false }` without logging
  // command bytes.
  function consume(shellEvent) {
    if (!shellEvent || typeof shellEvent.command !== "string") {
      return { consumed: false, reason: "missing-command" };
    }
    const nonce = parseManagedNonce(shellEvent.command);
    if (!nonce) return { consumed: false, reason: "no-header" };
    const current = now();
    const entry = entries.get(nonce);
    if (!entry) return { consumed: false, reason: "unknown-nonce" };
    // Single use: the entry is removed before any further check so a replay
    // can never pair again, even on mismatch.
    entries.delete(nonce);
    if (current - entry.timestamp > ttlMs) {
      return { consumed: false, reason: "expired" };
    }
    if (shellEvent.command !== entry.expected) {
      return { consumed: false, reason: "mismatch" };
    }
    const rewritten = shellEvent.command.slice(
      managedHeaderFor(nonce).length,
    );
    shellEvent.command = rewritten;
    let env = shellEvent.env;
    if (!env || typeof env !== "object") {
      env = {};
      shellEvent.env = env;
    }
    try {
      if (entry.role) {
        env[MANAGED_ROLE_ENV] = entry.role;
      } else {
        // Unknown role: clear any inherited role so the CLI cannot mistake
        // ambient state for a trusted identity, but still mark the shell as
        // managed so the CLI fails closed instead of becoming Operator.
        delete env[MANAGED_ROLE_ENV];
      }
      env[MANAGED_SESSION_ENV] = entry.sessionID;
    } catch (_) {
      return { consumed: false, reason: "env-failed" };
    }
    return { consumed: true, nonce, role: entry.role, sessionID: entry.sessionID };
  }

  function cleanupByToolCallId(toolCallId) {
    if (!isNonEmptyString(toolCallId)) return 0;
    let removed = 0;
    for (const [nonce, entry] of entries) {
      if (entry.toolCallId === toolCallId) {
        entries.delete(nonce);
        removed += 1;
      }
    }
    // Lazy expiry keeps the map bounded even when after-hooks never fire
    // (for example a cancelled tool call whose after event is unproven).
    pruneExpired(now());
    return removed;
  }

  function size() {
    pruneExpired(now());
    return entries.size;
  }

  function clear() {
    entries.clear();
  }

  return {
    decorate,
    consume,
    cleanupByToolCallId,
    size,
    clear,
    // Test-only introspection without exposing command bytes in logs.
    _entries: entries,
    _capacity: capacity,
    _ttlMs: ttlMs,
  };
}

// `shell.create.before` hook factory: property-only `event.command` /
// `event.env` edits, no global env mutation, no identity on failure.
export function createShellEnvHook(bridge) {
  return async function shellCreateBefore(event) {
    if (!event || typeof event.command !== "string") return;
    if (!bridge || typeof bridge.consume !== "function") return;
    try {
      bridge.consume(event);
    } catch (_) {
      // Never throw into a tool call; a failed consume simply leaves the
      // command and env untouched so the host marker fails closed.
    }
  };
}

// `tool.execute.after` hook factory: evicts by tool call id so cancelled or
// failed calls never leak. Carries no identity and logs no command data.
export function createToolAfterHook(bridge) {
  return async function executeAfter(event) {
    if (!event || !bridge || typeof bridge.cleanupByToolCallId !== "function") {
      return;
    }
    try {
      const toolCallId =
        typeof event.id === "string"
          ? event.id
          : event && event.input && typeof event.input === "object"
            ? null
            : null;
      // The host carries the same `id` on before and after for one tool
      // call; fall back to scanning nothing when the shape is unknown.
      if (toolCallId) bridge.cleanupByToolCallId(toolCallId);
    } catch (_) {
      // ignore: cleanup must never break a tool call
    }
  };
}
