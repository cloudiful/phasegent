import { expect, test } from "bun:test";
import plugin from "../phasegent-worktree.js";

// The generated plugin no longer embeds the protocol or role documents: one
// filesystem skill installed by `phasegent skill install` owns them. The plugin
// only prepends a short routing/safety hint to each protocol agent, so the
// always-on surface is minimal and every role loads its detail on demand.
test("generated plugin applies the routing hint across repeated bindings", async () => {
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
  const hint = plugin.redirect.SKILL_ROUTING_HINT;

  for (const role of ["executor", "reviewer", "explore", "orchestrator"]) {
    const system = agents.find((agent) => agent.id === role).system;
    expect(system.startsWith(hint)).toBe(true);
    expect(system).toContain(`Host ${role} instructions.`);
  }
});
