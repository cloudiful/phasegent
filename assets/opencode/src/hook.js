// The v2 `tool.execute.before` hook: one mutable event
// `{ tool, sessionID, agent, messageID, id, input }`; core continues with the
// returned `event.input` (packages/core/src/tool.ts:103-111, :271-280), so
// relative path arguments and a bare/relative shell `workdir` are rewritten in
// place. Failures stay silent passthrough: a failed lookup must not block the
// call.

import { rewritePhasegentCommand } from "./command.js";
import { ensureSessionWorktree } from "./discovery.js";
import { SHELL_TOOLS, redirectPaths } from "./paths.js";
import { sessionPlaced } from "./session.js";

export function createRedirectHook(context, deps) {
  return async function executeBefore(event) {
    const sessionId = event ? event.sessionID : undefined;
    const input = event ? event.input : undefined;
    const placedBefore = sessionPlaced(sessionId);
    let workdir = null;
    try {
      workdir = await ensureSessionWorktree(context, sessionId, event, deps);
    } catch (_) {
      workdir = null; // silent passthrough: a failed lookup must not block the call
    }
    if (!input || typeof input !== "object") return;
    if (SHELL_TOOLS.includes(event.tool) && typeof input.command === "string") {
      try {
        const rewritten = rewritePhasegentCommand(input.command, sessionId, event);
        if (rewritten !== input.command) input.command = rewritten;
      } catch (_) {
      }
    }
    if (typeof workdir !== "string" || workdir.length === 0) return;
    // A confirmed `session.move` already placed the session cwd in the worktree,
    // so relative paths resolve there on their own. The call that triggered the
    // move still runs in the old cwd, hence the `placedBefore` guard.
    if (placedBefore && sessionPlaced(sessionId)) return;
    const redirected = redirectPaths(event.tool, workdir, input);
    if (redirected === input) return;
    // Mutate in place: core keeps using `event.input`, and in-place writes keep
    // the object identity the caller already holds.
    for (const key of Object.keys(redirected)) input[key] = redirected[key];
  };
}
