import { expect, test } from "bun:test";
import plugin from "../phasegent-worktree.js";

// Mock turns verify prompt delivery and continuation context, not model compliance.
test("generated plugin preserves parent-first guidance across same-child blocker continuation", async () => {
  const agents = ["executor", "reviewer", "explore", "orchestrator"].map((id) => ({
    id, system: `Host ${id} instructions.`,
  }));
  const context = {
    agent: {
      transform: async (apply) => {
        apply({
          list: () => agents,
          update: (id, mutate) => mutate(agents.find((agent) => agent.id === id)),
        });
        return { dispose: async () => {} };
      },
    },
  };
  await plugin.redirect.registerAgentSkills(context);
  const shared = plugin.redirect.skillDefinition().content.replace(/\s+/g, " ");
  expect(shared).toContain("Pointers are not the detail, so the parent reads the record body");
  expect(shared).toContain("record it in the issue body before resuming the same child with a new attempt marker");
  const sessions = new Map();
  for (const role of ["executor", "reviewer", "explore"]) {
    const system = agents.find((agent) => agent.id === role).system;
    sessions.set(role, { id: `ses_mock_${role}`, system, turns: [] });
    const session = sessions.get(role);
    session.turns.push({ marker: `${role}-a1`, decision: null });
    const handoff = {
      unresolved: "The scoped checker cannot parse the source schema.",
      evidence: "The checker reads permission; the source uses permissions.",
      options: "Authorize a checker-only correction or leave source untouched.",
      scopeEffect: "Requires an explicit test/checker allowlist decision.",
      preserved: "Source files remain unchanged.",
    };
    expect(system.replace(/\s+/g, " ")).toMatch(/Never ask the human (through|via) the question tool/);
    expect(system).toContain("record body");
    expect(Object.keys(handoff)).toHaveLength(5);
    const bodyDecision = { allowlist: ["checker", "tests"], reason: handoff.evidence };
    session.turns.push({ marker: `${role}-a2`, delta: bodyDecision });
    expect(sessions.get(role)).toBe(session);
    expect(session.turns[1].marker).not.toBe(session.turns[0].marker);
    expect(session.system).toBe(system);
  }
  await plugin.redirect.registerAgentSkills(context);
  for (const role of ["executor", "reviewer", "explore"]) {
    expect(agents.find((agent) => agent.id === role).system).toBe(sessions.get(role).system);
  }
  const primary = agents.find((agent) => agent.id === "orchestrator").system.replace(/\s+/g, " ");
  expect(primary).toContain("Read the full record body, not just its pointer");
  expect(primary).toContain("wait for their answer before updating the body or resuming work");
  expect(primary).toContain("Never answer a child's native pending-question request");
});
