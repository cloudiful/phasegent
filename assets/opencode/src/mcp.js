// The phasegent MCP server registration.
//
// `registerPhasegentMcp` adds the local `phasegent mcp serve` process to the
// host's MCP state under the fixed name `phasegent`. The name matters: OpenCode
// exposes an MCP tool as
// `sanitize(server) + "_" + sanitize(tool)`
// (packages/opencode/src/mcp/catalog.ts:119), so the contracted tracking tools
// reach the model as `phasegent_issue_get` and friends.
//
// The server starts once with the fixed least-privilege role below, so no
// client can choose or elevate one.
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

export const PHASEGENT_MCP_SERVER = "phasegent";

// The role the MCP server process starts with. Fixed and least-privilege:
// `executor` carries the tracking surface (`issue_get`, `status_next`,
// `comment_create`, `notify_send`) without carrying the orchestrator's plan,
// status, timer, or worktree-lease powers. A model can never change it.
export const MCP_SERVER_ROLE = "executor";

// Whether the phasegent MCP server was registered for this plugin activation.
// Module state rather than a per-event argument, because registration happens
// once in `setup` and the entry's state must agree with it for every call.
let registered = false;

export function mcpRegistered() {
  return registered;
}

export function forgetMcpRegistration() {
  registered = false;
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
// host evidence). An unavailable, unrecognised, or rejected registration costs
// the tracking tools and nothing else.
export async function registerPhasegentMcp(context) {
  const mcp = context ? context.mcp : undefined;
  const transform = mcp && mcp.transform;
  if (typeof transform !== "function") {
    registered = false;
    warn(
      "phasegent: host exposes no mcp.transform; the phasegent MCP server stays unregistered",
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
            "unregistered",
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
  // the tools unavailable rather than advertising a server that cannot run.
  if (!present) {
    registered = false;
    warn(
      "phasegent: host mcp state does not carry the phasegent server after registration",
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
