// Shell command rewriting for `phasegent` invocations (issue #541; issue #588
// phase 2): the session role is exported per invocation through
// `PHASEGENT_ROLE`, `issue create|bind`/`issue close` get their session flag,
// and a sub-agent's `issue create|bind` is refused.

import { agentRole, isSubagentSession } from "./roles.js";
import { phasegentInvocation, shellSegments, transformCodeOnly } from "./scanner.js";
import { isWindowsHost, warn } from "./runtime.js";

export const PHASEGENT_ISSUE_WRITE = /\bissue\s+(create|bind)\b/;
// `issue close` names its closer through `--worktree-session`: the parser
// rejects `--session` there with `unknown option '--session'` (exit 2,
// src/command/issue.rs), so the segment gets the flag the CLI parses.
export const PHASEGENT_ISSUE_CLOSE = /\bissue\s+close\b/;
export const ROLE_ENV = "PHASEGENT_ROLE";
// The subexpression-local variable that holds the previous role while a
// PowerShell invocation runs; a `$( … )` subexpression runs in its own scope, so
// it never outlives the invocation.
const ROLE_SAVE = "$__phasegent_role";
// The CLI's exit status, captured before the restore assignment overwrites `$?`.
const ROLE_STATUS = "$__phasegent_status";
const SESSION_FLAG = /(^|\s)--session(\s|=)/;
const WORKTREE_SESSION_FLAG = /(^|\s)--worktree-session(\s|=)/;

// A role the segment already assigns, in either shell form. The quoted
// alternatives keep a value that merely contains the literal (for example
// `FOO="PHASEGENT_ROLE=x"`) from counting as an assignment.
const ROLE_ASSIGNMENT = /(?:^|[\s;&|(])(?:\$env:)?PHASEGENT_ROLE=("[^"]*"|'[^']*'|\S+)/g;

// The role scope of one invocation, as a `head` inserted before the command
// name and a `tail` inserted after its arguments.
//
// POSIX shells take a `PHASEGENT_ROLE=<role>` prefix, which the shell applies to
// the command's own process only. PowerShell has no equivalent inline prefix,
// and a bare `$env:PHASEGENT_ROLE='<role>';` assignment outlives the invocation:
// later statements and the child processes they launch inherit the role (issue
// #588 P2 review). The invocation is therefore wrapped in a `$( … )`
// subexpression that saves the previous value and restores it from `finally`, so
// the role is scoped to the CLI child even when the CLI fails.
//
// The wrapper must also preserve the CLI's failure status, or a failing
// invocation would look successful to `&&`/`||`: the successful restore
// assignment would be the last command and would reset `$?` to true. A
// subexpression keeps `finally` from masking the status, and the captured
// nonzero `$LASTEXITCODE` is re-asserted with a silent failure so the wrapper
// stays failed. `windows` is injectable so both forms stay testable from one
// host.
export function roleScope(role, windows) {
  const win = typeof windows === "boolean" ? windows : isWindowsHost();
  if (!win) return { head: `${ROLE_ENV}=${role} `, tail: "" };
  return {
    head: `$( ${ROLE_SAVE}=$env:${ROLE_ENV}; try { $env:${ROLE_ENV}='${role}'; `,
    tail:
      ` } finally { ${ROLE_STATUS}=$LASTEXITCODE; $env:${ROLE_ENV}=${ROLE_SAVE}; ` +
      `if (${ROLE_STATUS} -ne 0) { Write-Error -Message 'phasegent failed' -ErrorAction SilentlyContinue } } )`,
  };
}

// What the invocation's own prefix already assigns: `absent` when it carries no
// role, otherwise `matches`/`differs` against the session role.
export function roleAssignmentStatus(prefix, role) {
  let last = null;
  for (const match of prefix.matchAll(ROLE_ASSIGNMENT)) last = match[1];
  if (last === null) return "absent";
  return last.replace(/^["']|["']$/g, "") === role ? "matches" : "differs";
}

// The Windows scope head already injected before an invocation. Its role
// assignment sits before the `;` that opens the CLI statement, so it lives in an
// earlier shell segment than the token; a re-run must still recognize it or it
// would nest a second scope.
const WINDOWS_SCOPE_HEAD_RE =
  /\$\(\s*\$__phasegent_role=\$env:PHASEGENT_ROLE;\s*try\s*\{\s*\$env:PHASEGENT_ROLE=('[^']*'|\S+);\s*$/;

// The window of text the role check reads: the invocation's own segment, plus
// the scope head a previous Windows rewrite left just before it.
function roleStatusPrefix(source, segment, invocation, windows) {
  const base = source.slice(segment.start, invocation.tokenEnd);
  if (!windows) return base;
  const head = source.slice(0, segment.start).match(WINDOWS_SCOPE_HEAD_RE);
  return head ? `${head[0]}${base}` : base;
}

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

// The start of the `phasegent` token: scanning back over the characters the
// token may contain reproduces the span `phasegentInvocation` matched, so the
// role prefix lands immediately before the command name.
function tokenStartOf(text, tokenEnd) {
  let index = tokenEnd;
  while (index > 0 && !/[\s;&|()'"`]/.test(text[index - 1])) index -= 1;
  return index;
}

const SUBAGENT_REFUSAL =
  "echo \"phasegent: sub-agent sessions cannot run 'issue create|bind'; ask the orchestrator\" >&2; false";

export function rewritePhasegentCommand(command, sessionId, event, options) {
  if (typeof command !== "string" || !command.includes("phasegent")) return command;
  const role = agentRole(event);
  const subagent = isSubagentSession(event);
  const windows = options && typeof options.windows === "boolean" ? options.windows : isWindowsHost();
  let source = command;
  if (subagent) {
    // A sub-agent may not claim an elevated role: an unquoted claim becomes the
    // session role here, and a quoted one is outranked by the injected
    // assignment below (the last assignment wins in both shells).
    source = transformCodeOnly(source, (span) =>
      span.replace(
        /(^|[;&|()\s])(PHASEGENT_ROLE=)(orchestrator|admin)\b/gi,
        (_match, lead, env) => `${lead}${env}${role}`,
      ),
    );
    if (source !== command) {
      warn(`phasegent: sub-agent session cannot claim an orchestrator/admin role; using '${role}'`);
    }
  }
  const entries = shellSegments(source)
    .map((segment) => ({ segment, invocation: phasegentInvocation(segment) }))
    .filter((entry) => entry.invocation);
  if (entries.length === 0) return source;
  if (subagent && entries.some(({ invocation }) => PHASEGENT_ISSUE_WRITE.test(invocation.tail))) {
    warn("phasegent: sub-agent session refused 'issue create|bind' (orchestrator-only)");
    return SUBAGENT_REFUSAL;
  }
  let result = source;
  // Insert from the last invocation backwards so earlier offsets stay valid.
  for (let index = entries.length - 1; index >= 0; index -= 1) {
    const { segment, invocation } = entries[index];
    // An unterminated quote leaves no injection point that is provably outside
    // the value: that segment stays byte-for-byte.
    if (!invocation.balanced) continue;
    const tailFlags = [];
    const sessionFlag = sessionId ? sessionFlagFor(invocation.tail) : null;
    if (sessionFlag && !sessionFlag.present.test(invocation.tail)) {
      tailFlags.push(`${sessionFlag.name} ${sessionId}`);
    }
    let scope = null;
    if (role) {
      const prefix = roleStatusPrefix(source, segment, invocation, windows);
      const status = roleAssignmentStatus(prefix, role);
      // An explicit assignment wins, except that a sub-agent's differing claim
      // never outranks the managed session role.
      if (status === "absent" || (subagent && status === "differs")) {
        scope = roleScope(role, windows);
      }
    }
    const tail = `${tailFlags.length > 0 ? ` ${tailFlags.join(" ")}` : ""}${scope ? scope.tail : ""}`;
    if (tail) {
      result = `${result.slice(0, invocation.segmentEnd)}${tail}${result.slice(invocation.segmentEnd)}`;
    }
    if (scope) {
      const start = tokenStartOf(source, invocation.tokenEnd);
      result = `${result.slice(0, start)}${scope.head}${result.slice(start)}`;
    }
  }
  return result;
}
