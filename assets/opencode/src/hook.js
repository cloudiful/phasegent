// The v2 `tool.execute.before` hook: one mutable event
// `{ tool, sessionID, agent, messageID, id, input }`; core continues with the
// returned `event.input` and rethrows a hook error before the tool executes
// (packages/core/src/tool.ts:103-111, :271-280). Placement is move-only
// (issue 623): `ensureSessionWorktree` throws when a required `session.move`
// is unavailable, fails, or is still landing at a step boundary, so OpenCode
// cancels that invocation instead of running it against the old checkout; the
// next invocation retries the placement. Shell command rewriting (role/session
// injection) runs regardless of placement and stays independent of it.
//
// A registered MCP tool arrives as the same event with the mutable arguments
// under `output.args` instead of `input`
// (packages/opencode/src/session/tools.ts:105-112), so the host session binding
// reads whichever object the host exposed. Binding runs before the call either
// way: a delegation with no host session throws out of the hook, which cancels
// that invocation instead of sending it unbound.

import { rewritePhasegentCommand } from "./command.js";
import { ensureSessionWorktree } from "./discovery.js";
import { bindResearchSession } from "./mcp.js";
import { SHELL_TOOLS } from "./paths.js";

export function createRedirectHook(context, deps) {
  return async function executeBefore(event) {
    const sessionId = event ? event.sessionID : undefined;
    const input = event ? event.input : undefined;
    // A no-worktree session resolves to null and the call proceeds; a required
    // placement that is unavailable, fails, or is still landing throws out of
    // the hook, which is the host's contract for cancelling the pending call
    // (issue 623). The retry then runs placed.
    await ensureSessionWorktree(context, sessionId, event, deps);
    // Bind the calling host session into a research delegation. A non-research
    // tool is a no-op; a delegation with no host session throws and cancels.
    bindResearchSession(event);
    if (!input || typeof input !== "object") return;
    if (SHELL_TOOLS.includes(event.tool) && typeof input.command === "string") {
      try {
        const rewritten = rewritePhasegentCommand(input.command, sessionId, event);
        if (rewritten !== input.command) input.command = rewritten;
      } catch (_) {
      }
    }
  };
}
