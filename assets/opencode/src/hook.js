// The v2 runtime hooks: placement, and phasegent command rewriting.
//
// `context.session.hook("prompt", event)` receives the same mutable payload
// object for every submitted turn (`{ sessionID, messageID, prompt, metadata,
// delivery }`, core `SessionPrompt.prepare`). It owns the whole placement
// decision (issue 37): the session is registered, inherited, or discovered and
// then moved before the runner reaches any tool call, so the move lands at the
// runner's next step boundary with no in-flight invocation to protect and
// nothing to cancel. A host that cannot move the session still fails the prompt,
// so no turn runs unplaced.
//
// `context.tool.hook("execute.before", event)` receives
// `{ tool, sessionID, agent, messageID, id, input }`; core continues with the
// returned `event.input` and rethrows a hook error before the tool executes
// (packages/core/src/tool.ts:103-111, :271-280). It only rewrites shell
// commands, so phasegent invocations carry their role and session and a
// sub-agent cannot run the orchestrator-only `issue create`/`bind`. It holds no
// placement state and raises no placement error.

import { rewritePhasegentCommand } from "./command.js";
import { ensureSessionWorktree } from "./discovery.js";
import { SHELL_TOOLS } from "./paths.js";

export function createRedirectHook() {
  return async function executeBefore(event) {
    if (!event || typeof event.input !== "object") return;
    const command = event.input.command;
    if (!SHELL_TOOLS.includes(event.tool) || typeof command !== "string") return;
    try {
      const rewritten = rewritePhasegentCommand(command, event.sessionID, event);
      if (rewritten !== command) event.input.command = rewritten;
    } catch (_) {
    }
  };
}

// The prompt hook mutates the payload in place: the host keeps the object it
// passed, so the handler only has to read `sessionID`.
export function createPromptHook(context, deps) {
  return async function onPrompt(event) {
    if (!event) return;
    await ensureSessionWorktree(context, event.sessionID, deps);
  };
}
