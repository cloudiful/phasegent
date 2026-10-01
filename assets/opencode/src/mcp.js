// The phasegent MCP server registration and the host-bound research session.
//
// Two jobs, both about *who* is asking:
//
//  1. `registerPhasegentMcp` adds the local `phasegent mcp serve` process to the
//     host's MCP state under the fixed name `phasegent`. The name matters:
//     OpenCode exposes an MCP tool as
//     `sanitize(server) + "_" + sanitize(tool)`
//     (packages/opencode/src/mcp/catalog.ts:119), so the tools reach the model
//     as `phasegent_research_start` and friends and the bridge can recognise
//     them.
//  2. `bindResearchSession` runs from the `tool.execute.before` hook, which
//     OpenCode calls for registered MCP tools with the mutable `output.args`
//     just before `tools/call`
//     (packages/opencode/src/session/tools.ts:105-112). The bridge *overwrites*
//     the `session` argument with `event.sessionID`.
//
// The overwrite is the whole ownership story, so it is worth being explicit
// about what it buys. The model may put anything in `session`; the bridge
// discards it. The server therefore always learns the *calling* session, binds
// the run to it, and refuses status/wait/cancel/resume to any other session. A
// model cannot reach another session's run by naming one, and a run carries the
// durable owner that started it. There is no issue selector and no worktree
// lease: a research run always executes in a private server-created scratch
// directory, never in the phasegent repository or a resolved checkout.
//
// Two properties are deliberately not implemented here. The bridge never sends
// a location (the server creates the scratch directory; the model never sees
// it), and it never picks a role per call: the server starts once with the
// fixed least-privilege role below, so no client can choose or elevate one.
//
// The v2 surface this file targets, read off the shipped v2.0.18 host:
//
//   * Config: an MCP server lives at `mcp.servers.<name>`, and the flag that
//     keeps it connected is `disabled` (packages/core/src/config/mcp.ts:15-24).
//     `disabled: false` is therefore the entry's own state, not a separate
//     switch, and the entry carries no credential of its own.
//   * Plugin: `context.mcp.transform(draft => …)`, where the draft is
//     `{ list, get, set, update, remove }`, plus `context.mcp.reload()` to
//     connect whatever the transform added. The host's own config plugin feeds
//     `config.mcp.servers` through exactly this draft — `if (draft.get(name))
//     continue; draft.set(name, entry)`, then `mcp.reload()` — so those three
//     calls are the whole documented way in; the plugin context has no hook
//     that accepts a raw configuration object.
//
// The canonical chezmoi config carries the same entry, and the two agree: an
// entry that is already configured is left exactly as it is, so a host that
// reads the entry from its config never sees a second one from the plugin.

import { errorText, warn } from "./runtime.js";
import { isDelegatingSession } from "./roles.js";

export const PHASEGENT_MCP_SERVER = "phasegent";

// The role the MCP server process starts with. Fixed and least-privilege: the
// research surface is the only reason the server is registered, and `executor`
// carries it without carrying the orchestrator's plan, status, timer, or
// worktree-lease powers. A model can never change it.
export const MCP_SERVER_ROLE = "executor";

// The argument the bridge owns. The model may emit it; the bridge replaces it.
export const HOST_SESSION_FIELD = "session";

// The five delegation operations, in the delegation contract's order.
export const RESEARCH_ACTIONS = ["start", "status", "wait", "cancel", "resume"];

// The tool id OpenCode exposes for one research action.
export function researchToolId(action) {
  return `${PHASEGENT_MCP_SERVER}_research_${action}`;
}

// The research action a tool id names, or `null` for anything else — including
// a phasegent tool that is not part of the delegation surface.
export function researchActionForTool(tool) {
  if (typeof tool !== "string") return null;
  const prefix = `${PHASEGENT_MCP_SERVER}_research_`;
  if (!tool.startsWith(prefix)) return null;
  const action = tool.slice(prefix.length);
  return RESEARCH_ACTIONS.includes(action) ? action : null;
}

// Arguments the bridge removes before the call. None of them is part of the
// research tool contract, so a model that emits one is trying to reintroduce a
// location or an issue binding; dropping it keeps that attempt from ever
// reaching the server.
const FORBIDDEN_ARGUMENTS = [
  "issue",
  "worktree",
  "worktree_path",
  "worktreePath",
  "checkout_path",
  "repo_identity",
  "lease_id",
  "cwd",
  "directory",
  "path",
];

// Whether the phasegent MCP server was registered for this plugin activation.
// Module state rather than a per-event argument, because registration happens
// once in `setup` and the backend decision must agree with it for every call.
let registered = false;

export function mcpRegistered() {
  return registered;
}

export function forgetMcpRegistration() {
  registered = false;
}

// The error prefix for a delegation the bridge refuses. It mirrors the
// placement failure contract: a throw from the hook is the host's way of
// cancelling one invocation, so the call is cancelled instead of being sent
// without a host identity.
export const BINDING_ERROR_PREFIX = "phasegent: research session binding";

// The refusal reasons, kept as a closed set so a caller can branch and a test
// can assert them.
export const REFUSALS = {
  NOT_RESEARCH: "not-a-research-tool",
  NO_SESSION: "no-host-session",
  NO_ARGS: "no-mutable-arguments",
};

// Bind the calling host session into a research tool's arguments.
//
// Returns a decision rather than throwing, except for the refusal cases: a
// missing host session or a hook event with no mutable argument object throws
// with `BINDING_ERROR_PREFIX`, which cancels the pending call. A non-research
// tool is a no-op, and so is a different phasegent tool.
export function bindResearchSession(event) {
  const action = researchActionForTool(event && event.tool);
  if (action === null) return { bound: false, reason: REFUSALS.NOT_RESEARCH, action: null };
  const sessionId = event ? event.sessionID : undefined;
  if (typeof sessionId !== "string" || sessionId.trim().length === 0) {
    throw new Error(
      `${BINDING_ERROR_PREFIX} refused: this invocation carries no host session id, so the ` +
        `research ${action} call cannot be bound to its owner and is cancelled`,
    );
  }
  // OpenCode passes the mutable arguments as `output.args` for an MCP call
  // (packages/opencode/src/session/tools.ts:105-112). `input` is the shape the
  // local-tool path uses, so it is accepted as the fallback rather than
  // assuming one host build.
  const args = mutableArguments(event);
  if (!args) {
    throw new Error(
      `${BINDING_ERROR_PREFIX} refused: the research ${action} call exposed no mutable ` +
        "arguments, so the host session could not be bound and the call is cancelled",
    );
  }
  for (const key of FORBIDDEN_ARGUMENTS) delete args[key];
  // The single overwrite that makes the delegation authorized: whatever the
  // model emitted here is discarded and replaced with this host session.
  args[HOST_SESSION_FIELD] = sessionId.trim();
  return { bound: true, reason: null, action };
}

// The mutable argument object for one hook event, or `null` when the host
// exposed none.
export function mutableArguments(event) {
  if (!event || typeof event !== "object") return null;
  if (event.output && typeof event.output === "object" && event.output.args) {
    return event.output.args;
  }
  if (event.input && typeof event.input === "object") {
    return event.input;
  }
  return null;
}

// The local MCP server entry phasegent needs: the CLI in stdio mode, with the
// fixed role, no credential of its own, and `disabled: false` so the host
// connects it. Returns a fresh object each call so a host that mutates the
// entry cannot poison the next attempt.
export function phasegentMcpServerDefinition() {
  return {
    type: "local",
    command: ["phasegent", "mcp", "serve", "--transport", "stdio"],
    environment: {
      PHASEGENT_ROLE: MCP_SERVER_ROLE,
    },
    disabled: false,
  };
}

// Whether the host MCP state already carries the phasegent server, so a second
// registration is a no-op instead of a duplicate entry.
//
// Two representations of the same v2 shape are accepted: a host draft (the
// `get`/`set` pair the host's own config plugin writes through) and a plain v2
// configuration object read as `mcp.servers.<name>` — the form the canonical
// chezmoi config writes. Neither is a guess about a host API; both are the
// documented shape, one mutable at runtime and one on disk.
export function hasPhasegentServer(config) {
  if (!config || typeof config !== "object") return false;
  if (typeof config.get === "function" && typeof config.set === "function") {
    return Boolean(config.get(PHASEGENT_MCP_SERVER));
  }
  const servers = config.mcp && config.mcp.servers;
  if (!servers || typeof servers !== "object") return false;
  return Object.prototype.hasOwnProperty.call(servers, PHASEGENT_MCP_SERVER);
}

// Add the server entry to a host MCP draft, without clobbering an entry a user
// or the canonical config already provided. Returns whether the entry is
// present in the draft afterwards, so a caller can tell "the host has it" from
// "the host accepted the call and dropped the entry".
export function applyServer(draft, definition) {
  if (!draft || typeof draft !== "object") return false;
  // `get` and `set` are the two draft calls the host's own config plugin
  // makes; a draft without both is a shape this bridge does not understand.
  if (typeof draft.get !== "function" || typeof draft.set !== "function") return false;
  if (draft.get(PHASEGENT_MCP_SERVER)) return true;
  draft.set(PHASEGENT_MCP_SERVER, definition);
  return Boolean(draft.get(PHASEGENT_MCP_SERVER));
}

// Add the phasegent MCP server to the host's runtime MCP state.
//
// Every failure is a warning and a no-op, never a throw: a throw inside a
// plugin callback disables the whole plugin, redirect hook included (issue #533
// host evidence). The native `explore` path stays the fallback either way, so
// an unavailable, unrecognised, or rejected registration costs the delegation
// and nothing else.
export async function registerPhasegentMcp(context) {
  const mcp = context ? context.mcp : undefined;
  const transform = mcp && mcp.transform;
  if (typeof transform !== "function") {
    registered = false;
    warn(
      "phasegent: host exposes no mcp.transform; the phasegent MCP server stays unregistered " +
        "and research delegation uses the native path",
    );
    return { registered: false, reason: "no-mcp-transform" };
  }
  const definition = phasegentMcpServerDefinition();
  let present = false;
  try {
    await transform((draft) => {
      if (!draft || typeof draft.get !== "function" || typeof draft.set !== "function") {
        warn(
          "phasegent: host mcp draft exposes no get/set; the phasegent MCP server stays " +
            "unregistered and research delegation uses the native path",
        );
        return;
      }
      try {
        present = applyServer(draft, definition);
      } catch (error) {
        warn(`phasegent: MCP server registration was rejected (${errorText(error)})`);
      }
    });
  } catch (error) {
    registered = false;
    warn(`phasegent: MCP server registration was rejected (${errorText(error)})`);
    return { registered: false, reason: "mcp-transform-failed" };
  }
  // The entry is only "registered" once the host's own state reports it, which
  // is the same test the host's config plugin relies on. Anything else leaves
  // the delegation unavailable rather than advertising tools that cannot run.
  if (!present) {
    registered = false;
    warn(
      "phasegent: host mcp state does not carry the phasegent server after registration; " +
        "research delegation uses the native path",
    );
    return { registered: false, reason: "entry-not-confirmed" };
  }
  // The host connects an added server on a domain reload, exactly as its own
  // config plugin does after writing its entries.
  if (typeof mcp.reload === "function") {
    try {
      await mcp.reload();
    } catch (error) {
      warn(`phasegent: MCP server reload was rejected (${errorText(error)})`);
    }
  }
  registered = true;
  return { registered: true, reason: null };
}

// The delegation backend for one host event: the phasegent ACP path when the
// server is registered, the caller is a role whose MCP surface carries the
// delegation, and the host supplied a session; otherwise the native OpenCode
// `explore` subagent. The reason is returned rather than only logged, because a
// caller has to be able to say which path it took.
//
// The delegating session is the caller: ownership is bound to the session that
// starts the run, not to the research child, so every later status/wait/cancel/
// resume for that run resolves the same caller.
export function researchBackend(event, registered) {
  if (!registered) return { backend: "native", reason: "mcp-not-registered" };
  if (!isDelegatingSession(event)) {
    return { backend: "native", reason: "not-a-delegating-role" };
  }
  const sessionId = event ? event.sessionID : undefined;
  if (typeof sessionId !== "string" || sessionId.trim().length === 0) {
    return { backend: "native", reason: "no-host-session" };
  }
  return { backend: "phasegent", reason: null, sessionId: sessionId.trim() };
}
