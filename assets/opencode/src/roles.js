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

// The agent id the host reports for the current call, lowercased. Kept separate
// from `agentRole` because the delegation gate is about which agent is allowed
// to hand research to the phasegent MCP server, not about which capability role
// the call is rewritten to.
export function agentName(event) {
  return event && typeof event.agent === "string" ? event.agent.toLowerCase() : "";
}

// The agent roles whose MCP surface carries the research delegation. This
// mirrors the server-side gate — the delegation is open to the orchestrator,
// executor, and reviewer and denied to `tester` and `admin` — so the bridge
// never offers a delegation the server would refuse. Matching is on the agent
// id rather than on `agentRole`, because `explore` maps to the reviewer
// capability surface while staying a non-delegating agent: it is the native
// fallback target, not a delegator.
export const DELEGATING_AGENTS = ["orchestrator", "executor", "reviewer"];

export function isDelegatingSession(event) {
  const agent = agentName(event);
  if (!agent) return false;
  return DELEGATING_AGENTS.some((hint) => agent.includes(hint));
}
