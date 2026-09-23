// v2 skill.transform: the embedded `phasegent` skills.
//
// The live v2.0.11 runtime draft is `{ list, get, add, update, remove }` and
// `add` takes a flat `Skill.Info` — the `{ id, name, description, path,
// content }` shape the host's own builtin skills use. The typed SDK's
// `source({ type: "embedded", skill })` draft is not part of the runtime, and
// calling a missing draft method kills the whole plugin activation, so the
// callback probes for `add` and warns instead of throwing.
//
// The skill bodies are the four protocol markdown files in
// `skills/phasegent/`, imported as text at build time: the generated dist
// carries the bytes, the runtime never reads a file, and editing a prompt means
// editing the markdown and rerunning `bun run build:plugin`. An embedded skill
// is delivered with the plugin, so it is visible on any host the adapter is
// installed on. `path` is the synthetic built-in path core uses for embedded
// skills.

import SKILL_CONTENT from "../../../skills/phasegent/SKILL.md" with { type: "text" };
import SKILL_ORCHESTRATOR_CONTENT from "../../../skills/phasegent/SKILL.orchestrator.md" with { type: "text" };
import SKILL_EXECUTOR_CONTENT from "../../../skills/phasegent/SKILL.executor.md" with { type: "text" };
import SKILL_REVIEWER_CONTENT from "../../../skills/phasegent/SKILL.reviewer.md" with { type: "text" };

import { errorText, warn } from "./runtime.js";

export const SKILL_ID = "phasegent";
export const SKILL_PATH = "/builtin/phasegent.md";

export const ROLE_SKILLS = [
  {
    id: "phasegent-orchestrator",
    path: "/builtin/phasegent-orchestrator.md",
    content: SKILL_ORCHESTRATOR_CONTENT,
  },
  {
    id: "phasegent-executor",
    path: "/builtin/phasegent-executor.md",
    content: SKILL_EXECUTOR_CONTENT,
  },
  {
    id: "phasegent-reviewer",
    path: "/builtin/phasegent-reviewer.md",
    content: SKILL_REVIEWER_CONTENT,
  },
];

// The host lists this description in the skill index, so it is derived from the
// embedded frontmatter: the listed skill and its body can never disagree.
export function frontmatterDescription(content) {
  const match = String(content).match(/^description:[ \t]*(.+)$/m);
  return match ? match[1].trim() : "";
}

export function skillDefinition() {
  return {
    id: SKILL_ID,
    name: SKILL_ID,
    path: SKILL_PATH,
    description: frontmatterDescription(SKILL_CONTENT),
    content: SKILL_CONTENT,
  };
}

export function roleSkillDefinitions() {
  return ROLE_SKILLS.map(({ id, path, content }) => ({
    id,
    name: id,
    path,
    description: frontmatterDescription(content),
    content,
  }));
}

export function skillDefinitions() {
  return [skillDefinition(), ...roleSkillDefinitions()];
}

export async function registerSkill(context) {
  const skill = context && context.skill;
  const transform = skill && skill.transform;
  if (typeof transform !== "function") {
    warn("phasegent: host exposes no skill.transform; the phasegent skill stays unregistered");
    return null;
  }
  const definitions = skillDefinitions();
  return await transform((draft) => {
    // A throw inside a transform callback disables the whole plugin (redirect
    // hook included), so an unknown draft shape only warns.
    if (!draft || typeof draft.add !== "function") {
      warn("phasegent: host skill draft exposes no add; the phasegent skill stays unregistered");
      return;
    }
    try {
      for (const definition of definitions) draft.add(definition);
    } catch (error) {
      warn(`phasegent: skill registration was rejected (${errorText(error)})`);
    }
  });
}
