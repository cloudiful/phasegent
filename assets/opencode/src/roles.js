// Agent-role mapping for session events (issue #541).
//
// `event.agent` is the agent id the host reports for the current tool call, and
// the role decides command rewriting: the per-invocation `PHASEGENT_ROLE` scope,
// a sub-agent's refusal of the orchestrator-only `issue create`/`bind`, and the
// downgrade of a claimed elevated role. `explore` is recon-only and maps to the
// reviewer capability surface.

export const AGENT_ROLE_HINTS = [
  ["orchestrator", "orchestrator"],
  ["executor", "executor"],
  ["reviewer", "reviewer"],
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

// The agent id the host reports for the current call, lowercased.
export function agentName(event) {
  return event && typeof event.agent === "string" ? event.agent.toLowerCase() : "";
}
