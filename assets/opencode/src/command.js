// Shell command rewriting for `phasegent` invocations (issue #541): the
// session role is injected as `--role`, `issue create|bind`/`issue close` get
// their session flag, and a sub-agent's `issue create|bind` is refused.

import { agentRole, isSubagentSession } from "./roles.js";
import { phasegentInvocation, shellSegments, transformCodeOnly } from "./scanner.js";
import { warn } from "./runtime.js";

export const PHASEGENT_ISSUE_WRITE = /\bissue\s+(create|bind)\b/;
// `issue close` names its closer through `--worktree-session`: the parser
// rejects `--session` there with `unknown option '--session'` (exit 2,
// src/command/issue.rs), so the segment gets the flag the CLI parses.
export const PHASEGENT_ISSUE_CLOSE = /\bissue\s+close\b/;
const ROLE_FLAG = /(^|\s)--role(\s|=)/;
const SESSION_FLAG = /(^|\s)--session(\s|=)/;
const WORKTREE_SESSION_FLAG = /(^|\s)--worktree-session(\s|=)/;

// The session-bearing flag of a segment: `issue create|bind` carry `--session`,
// `issue close` carries `--worktree-session`; every other segment stays
// untouched.
function sessionFlagFor(tail) {
  if (PHASEGENT_ISSUE_WRITE.test(tail)) return { name: "--session", present: SESSION_FLAG };
  if (PHASEGENT_ISSUE_CLOSE.test(tail)) {
    return { name: "--worktree-session", present: WORKTREE_SESSION_FLAG };
  }
  return null;
}

const SUBAGENT_REFUSAL =
  "echo \"phasegent: sub-agent sessions cannot run 'issue create|bind'; ask the orchestrator\" >&2; false";

export function rewritePhasegentCommand(command, sessionId, event) {
  if (typeof command !== "string" || !command.includes("phasegent")) return command;
  const role = agentRole(event);
  const subagent = isSubagentSession(event);
  let source = command;
  if (subagent) {
    source = transformCodeOnly(source, (span) =>
      span
        .replace(/(^|\s)--role(\s+|=)(orchestrator|admin)\b/g, (_m, lead, sep) => `${lead}--role${sep}${role}`)
        .replace(/(^|[;&|()\s])(PHASEGENT_ROLE=)(orchestrator|admin)\b/g, (_m, lead, env) => `${lead}${env}${role}`),
    );
    if (source !== command) {
      warn(`phasegent: sub-agent session cannot claim an orchestrator/admin role; using '${role}'`);
    }
  }
  const invocations = shellSegments(source).map(phasegentInvocation).filter(Boolean);
  if (invocations.length === 0) return source;
  if (subagent && invocations.some((invocation) => PHASEGENT_ISSUE_WRITE.test(invocation.tail))) {
    warn("phasegent: sub-agent session refused 'issue create|bind' (orchestrator-only)");
    return SUBAGENT_REFUSAL;
  }
  let result = source;
  // Insert from the last invocation backwards so earlier offsets stay valid.
  for (let index = invocations.length - 1; index >= 0; index -= 1) {
    const invocation = invocations[index];
    // An unterminated quote leaves no injection point that is provably outside
    // the value: that segment stays byte-for-byte.
    if (!invocation.balanced) continue;
    const tailFlags = [];
    const headFlags = [];
    if (role && !ROLE_FLAG.test(invocation.tail)) headFlags.push(`--role ${role}`);
    const sessionFlag = sessionId ? sessionFlagFor(invocation.tail) : null;
    if (sessionFlag && !sessionFlag.present.test(invocation.tail)) {
      tailFlags.push(`${sessionFlag.name} ${sessionId}`);
    }
    if (tailFlags.length > 0) {
      result = `${result.slice(0, invocation.segmentEnd)} ${tailFlags.join(" ")}${result.slice(invocation.segmentEnd)}`;
    }
    if (headFlags.length > 0) {
      result = `${result.slice(0, invocation.tokenEnd)} ${headFlags.join(" ")}${result.slice(invocation.tokenEnd)}`;
    }
  }
  return result;
}
