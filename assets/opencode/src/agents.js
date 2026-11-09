// v2 agent.transform: prepend one short routing/safety hint to each protocol
// agent (issue #22 P2).
//
// The full role documents are filesystem references installed by
// `phasegent skill install` and load on demand. The plugin injects only a
// minimal hint (how to load the skill and what never counts as identity), so
// every agent's system prompt stays lean and no role document is duplicated or
// made stale here.
//
// The live v2.0.12 draft is `{ list, get, default, update, remove }` with
// `update(id, mutate)` mutating the live agent info. The host resolves plugin
// `setup` before it materializes the configured agents and never re-runs an
// already-registered callback, so the binding runs one attempt inline and the
// rest detached on a bounded backoff, replacing the previous callback instead
// of stacking it. A host whose draft has no `update` keeps the hint inert and
// warns.

import { SKILL_ROUTING_HINT } from "./skills.js";
import { errorText, warn } from "./runtime.js";

// Exact protocol agent ids. A delegated child uses the same id, so the hint is
// stable across primary and child sessions.
export const PROTOCOL_AGENTS = ["orchestrator", "executor", "reviewer", "explore"];

// Total worst case ~0.8s, and only when the host never materializes a protocol
// agent (a host without the phasegent agents at all).
export const AGENT_BIND_BACKOFF_MS = [0, 10, 25, 50, 100, 200, 400];

export function isProtocolAgent(agentId) {
  const agent = typeof agentId === "string" ? agentId.toLowerCase() : "";
  return PROTOCOL_AGENTS.includes(agent);
}

// Idempotent prefixing: the host re-runs the callback on every agent-state
// invalidation, and a repeated pass must not stack the hint.
export function withSkillPrefix(system, content) {
  const current = typeof system === "string" ? system : "";
  if (current.startsWith(content)) return current;
  return current.length > 0 ? `${content}\n${current}` : content;
}

// One registration pass: prepend the hint to each present protocol agent and
// report how many protocol agents were seen and how many are bound now.
export function bindPresentAgents(draft) {
  const protocol = [];
  for (const info of draft.list()) {
    const id = info && typeof info.id === "string" ? info.id : "";
    if (isProtocolAgent(id)) protocol.push(id);
  }
  let bound = 0;
  for (const id of protocol) {
    draft.update(id, (entry) => {
      if (!entry || typeof entry !== "object") return;
      entry.system = withSkillPrefix(entry.system, SKILL_ROUTING_HINT);
    });
    bound += 1;
  }
  return { protocol, bound };
}

export async function registerAgentSkills(context, deps) {
  const agent = context && context.agent;
  const transform = agent && agent.transform;
  if (typeof transform !== "function") {
    warn("phasegent: host exposes no agent.transform; the routing hint stays uninjected");
    return null;
  }
  const backoff = (deps && deps.backoff) || AGENT_BIND_BACKOFF_MS;
  const wait = (deps && deps.wait) || ((ms) => new Promise((resolve) => setTimeout(resolve, ms)));
  const holder = { registration: null, disposed: false, settled: false, bound: 0, seen: new Set() };

  const runAttempt = async () => {
    if (holder.disposed) return;
    const previous = holder.registration;
    holder.registration = await transform((draft) => {
      if (!draft || typeof draft.list !== "function" || typeof draft.update !== "function") {
        warn("phasegent: host agent draft exposes no update; the routing hint stays uninjected");
        return;
      }
      try {
        const pass = bindPresentAgents(draft);
        for (const id of pass.protocol) holder.seen.add(id);
        holder.bound = pass.bound;
      } catch (error) {
        warn(`phasegent: routing hint injection was rejected (${errorText(error)})`);
      }
    });
    // Replace the stale callback: it would otherwise keep re-running on every
    // agent-state invalidation for the lifetime of the plugin.
    if (previous && typeof previous.dispose === "function") {
      try {
        await previous.dispose();
      } catch (_) {
        // A failed dispose only leaves an extra inert callback registered.
      }
    }
    if (holder.bound > 0 && holder.bound >= holder.seen.size) holder.settled = true;
  };

  await runAttempt();
  // The host resolves `setup` first and materializes the configured agents
  // afterwards; a callback registered while setup is still awaiting therefore
  // only ever sees the built-in agents, and the host never re-runs it for a
  // later load. The remaining attempts run detached on a bounded backoff so
  // setup stays non-blocking, and they stop as soon as every protocol agent
  // that appeared is bound.
  const settle = (async () => {
    for (let index = 1; index < backoff.length && !holder.settled; index += 1) {
      await wait(backoff[index]);
      if (holder.disposed) return;
      await runAttempt();
    }
    if (!holder.settled && holder.bound === 0) {
      warn("phasegent: no protocol agent was found; the routing hint stays uninjected");
    }
  })().catch((error) => {
    warn(`phasegent: routing hint binding failed (${errorText(error)})`);
  });

  return {
    settle,
    async dispose() {
      holder.disposed = true;
      const registration = holder.registration;
      if (registration && typeof registration.dispose === "function") {
        await registration.dispose();
      }
    },
  };
}
