// OpenCode v2 worktree adapter — source entry.
//
// The deployable artifact is the checked-in single file
// `assets/opencode/phasegent-worktree.js`, generated from this tree by
// `bun run build:plugin` (see `build.js`); prompt bodies are inlined at build
// time from `skills/phasegent/*.md`, and the runtime never reads a file.
//
// OpenCode >= 2.0 is required: the v1 plugin shape is rejected by the v2 module
// loader (`PluginModule.LoadError: Plugin must export a default definition with
// an id and an effect or setup function.`,
// packages/core/src/plugin/module.ts:60-73, :111). The v2 contract is
// `export default { id, setup }`; `setup(context)` registers hooks imperatively
// and returns a cleanup (packages/plugin/src/promise/plugin.ts:56-61).
//
//   * `context.session.hook("prompt", event)` — the same mutable payload object
//     for every submitted turn (`{ sessionID, messageID, prompt, metadata,
//     delivery }`, core `SessionPrompt.prepare`). It owns worktree placement
//     (issue 37): a session is registered, inherited, or discovered and then
//     moved before the runner reaches any tool call, so a Task-spawned child
//     follows its parent's directory and no tool invocation is ever cancelled to
//     move a session.
//   * `context.tool.hook("execute.before", event)` — one mutable event
//     `{ tool, sessionID, agent, messageID, id, input }`; core continues with the
//     returned `event.input` (packages/core/src/tool.ts:103-111, :271-280). It
//     only rewrites shell commands, so role/session injection stays available
//     and path rewriting stays absent.
//   * `context.worktree.transform(editor => editor.add({ id, create, remove, list }))`
//     (packages/plugin/src/promise/worktree.ts:5-22). It has no v1 `target`
//     callback, so the acquired worktree becomes the session directory through
//     `context.session.move` (packages/core/src/session/move.ts:41-49). The strategy
//     is only claimed when the checkout already carries a phasegent issue binding,
//     so a non-phasegent project keeps the host git strategy.
//   * `context.skill.transform(draft => draft.add({ id, name, description, path, content }))`
//     registers the embedded `phasegent` skill. The live v2.0.11
//     runtime draft is `{ list, get, add, update, remove }`, and `add` takes the
//     same flat `Skill.Info` the host's builtin skills use; the typed SDK's
//     `source({ type: "embedded", skill })` draft does not exist at runtime.
//   * `context.agent.transform(draft => draft.update(id, mutate))` binds each
//     protocol agent to its own slim skill: the skill body is prepended to the
//     agent's `system`, so the role skill is the stable, cacheable prefix of
//     that agent's system prompt. The live v2.0.12 draft is
//     `{ list, get, default, update, remove }`, and `update` mutates the live
//     agent info (its `system` included). The callback is re-run whenever the
//     agent state invalidates, so the prefix is applied idempotently.
//   * No slash command: the live v2.0.11 command draft exposes only
//     `add({ name, description, execute })`, and `execute` must return an
//     Effect that a promise plugin cannot build. Registering through the
//     typed SDK's `update(name, mutate)` draft raised a `TypeError` and the
//     host then disabled the whole plugin, redirect hook included (issue #533
//     host evidence), so `phasegent worktree acquire` stays the manual path.
//
// The npm `@opencode-ai/plugin` type package can lag the binary it ships with
// (`tool`, `worktree`, `session` and `location` are absent from 1.18.25 while
// the binary exposes them); the adapter targets the binary's runtime context.
//
// Session identity is `event.sessionID`, and a Task-spawned child reads its
// `parentID` through `context.session.get` in the prompt hook so it inherits the
// parent's directory instead of guessing from the most recently remembered
// worktree (session.js). Degradation is deliberate: no binding or a failed
// acquire keeps the original directory and warns on the console (v2 has no
// structured warning channel). A lease row that `issue close` / `issue sync`
// converged keeps its "issue closed…" release reason, so the lazy path refuses to
// acquire a fresh worktree for an already closed issue and stays in place (issue
// #575 P2). The host is also the authority that outranks the registry: `issue
// close` returns the closing session to the main checkout through the OpenCode
// API before removing its worktree, and a settled placement is reconciled against
// a fresh host read so that relocation is never reversed and a removed worktree is
// never re-entered by this session or a later child (issue #747 P2,
// session-reconcile.js). Absolute paths pass through untouched, so an explicit
// escape and the external_directory check that guards it are never rewritten. All
// worktree calls stay local: no network, no credentials, no .env copies. Branches
// and directories are never deleted here; removal is `phasegent worktree prune`.
import {
  acquireWorktree,
  issueClosedLocally,
  pickActiveWorktreePath,
  readBranchBinding,
  readIssueLeaseHistory,
  readIssueLeases,
} from "./binding.js";
import { rewritePhasegentCommand } from "./command.js";
import { discoverWorktreeForSession, ensureSessionWorktree } from "./discovery.js";
import { createPromptHook, createRedirectHook } from "./hook.js";
import { isAbsolutePath, redirectPathValue, redirectPaths } from "./paths.js";
import { agentName, agentRole, isSubagentSession } from "./roles.js";
import { errorText, warn } from "./runtime.js";
import {
  inheritedWorktree,
  moveSessionToWorktree,
  readAuthoritativeDirectory,
  readSessionInfo,
  rememberWorktree,
  resetWorktrees,
  sessionPlaced,
  worktreeForSession,
} from "./session.js";
import { reconcileSessionLocation } from "./session-reconcile.js";
import { registerAgentSkills, roleSkillContent, roleSkillId, withSkillPrefix } from "./agents.js";
import { registerSkill, roleSkillDefinitions, skillDefinition, skillDefinitions } from "./skills.js";
import {
  gitWorktreeAdd,
  registerWorktreeStrategy,
  worktreeStrategyDefinition,
} from "./strategy.js";

const PhasegentWorktreePlugin = {
  id: "phasegent-worktree",
  async setup(context) {
    const registrations = [];
    try {
      const strategy = await registerWorktreeStrategy(context);
      if (strategy) registrations.push(strategy);
    } catch (error) {
      warn(`phasegent: worktree strategy registration failed (${errorText(error)})`);
    }
    try {
      // The prompt hook owns placement: it is admitted before the runner reaches
      // any tool call, so no tool invocation is cancelled to move a session.
      const promptHook = context && context.session && context.session.hook;
      if (typeof promptHook === "function") {
        registrations.push(await promptHook("prompt", createPromptHook(context)));
      } else {
        warn("phasegent: host exposes no session prompt hook; worktree placement is disabled");
      }
    } catch (error) {
      warn(`phasegent: session.prompt registration failed (${errorText(error)})`);
    }
    try {
      // The tool hook only rewrites phasegent commands, so it carries no
      // placement state and cannot raise a placement error.
      const hook = context && context.tool && context.tool.hook;
      if (typeof hook === "function") {
        registrations.push(await hook("execute.before", createRedirectHook()));
      } else {
        warn("phasegent: host exposes no tool hook; command rewriting is disabled");
      }
    } catch (error) {
      warn(`phasegent: tool.execute.before registration failed (${errorText(error)})`);
    }
    try {
      const skill = await registerSkill(context);
      if (skill) registrations.push(skill);
    } catch (error) {
      warn(`phasegent: skill.transform registration failed (${errorText(error)})`);
    }
    try {
      const bound = await registerAgentSkills(context);
      if (bound) registrations.push(bound);
    } catch (error) {
      warn(`phasegent: agent.transform registration failed (${errorText(error)})`);
    }
    return async () => {
      for (const registration of registrations) {
        try {
          if (registration && typeof registration.dispose === "function") {
            await registration.dispose();
          }
        } catch (_) {
        }
      }
    };
  },
};

PhasegentWorktreePlugin.redirect = Object.freeze({
  isAbsolutePath,
  redirectPathValue,
  redirectPaths,
  agentName,
  agentRole,
  isSubagentSession,
  sessionPlaced,
  rewritePhasegentCommand,
  rememberWorktree,
  worktreeForSession,
  resetWorktrees,
  createRedirectHook,
  createPromptHook,
  pickActiveWorktreePath,
  issueClosedLocally,
  discoverWorktreeForSession,
  ensureSessionWorktree,
  moveSessionToWorktree,
  readSessionInfo,
  readAuthoritativeDirectory,
  inheritedWorktree,
  reconcileSessionLocation,
  readBranchBinding,
  acquireWorktree,
  readIssueLeases,
  readIssueLeaseHistory,
  registerWorktreeStrategy,
  worktreeStrategyDefinition,
  gitWorktreeAdd,
  registerSkill,
  skillDefinition,
  roleSkillDefinitions,
  skillDefinitions,
  registerAgentSkills,
  roleSkillId,
  roleSkillContent,
  withSkillPrefix,
});

export default PhasegentWorktreePlugin;
