// Agent-role mapping for session events (issue #541; issue #859 P1 exact match).
//
// `event.agent` is the agent id the host reports for the current tool call, and
// the role decides command rewriting: the per-invocation `PHASEGENT_ROLE` scope,
// a sub-agent's refusal of the orchestrator-only `issue create`/`bind`, and the
// downgrade of a claimed elevated role. `explore` maps to its own least-privilege
// role (issue #754 P3): the CLI grants it issue read plus structured `record`
// read, and an authorized recon `record create` only — never the reviewer
// surface, and never ordinary comment/status/admin/lease writes.
//
// Canonical IDs are exact after lowercasing: orchestrator, executor, reviewer,
// explore only (issue #859 P1). Compound or substring lookalikes
// (`task-executor`, `my-orchestrator`, `reviewer-2`) are unknown and get no
// role, so an unknown custom agent never elevates to a privileged role. An
// unknown agent still receives the managed session marker through the shell
// bridge and fails closed as managed-without-role instead of Operator.

export const AGENT_ROLE_HINTS = [
  ["orchestrator", "orchestrator"],
  ["executor", "executor"],
  ["reviewer", "reviewer"],
  ["explore", "explore"],
];

export function agentRole(event) {
  const agent = event && typeof event.agent === "string" ? event.agent.toLowerCase() : "";
  if (!agent) return null;
  for (const [hint, role] of AGENT_ROLE_HINTS) {
    if (agent === hint) return role;
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
