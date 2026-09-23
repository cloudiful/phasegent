// Agent-role mapping for session events (issue #541).
//
// `event.agent` is the agent id the host reports for the current tool call;
// the role decides both command rewriting and whether the session may acquire
// a worktree. `explore` is recon-only and maps to the reviewer capability
// surface.

export const AGENT_ROLE_HINTS = [
  ["orchestrator", "orchestrator"],
  ["executor", "executor"],
  ["reviewer", "reviewer"],
  ["tester", "tester"],
  ["explore", "reviewer"],
];

export function agentRole(event) {
  const agent = event && typeof event.agent === "string" ? event.agent.toLowerCase() : "";
  if (!agent) return null;
  for (const [hint, role] of AGENT_ROLE_HINTS) {
    if (agent.includes(hint)) return role;
  }
  return null;
}

export function isSubagentSession(event) {
  const role = agentRole(event);
  return role !== null && role !== "orchestrator";
}
