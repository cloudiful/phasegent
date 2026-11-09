// Shell command rewriting for `phasegent` invocations (issue #541; issue #588
// phase 2; issue #22 P1 verified identity; issue #22 P3 visible-mutation guard).
//
// The session role is never written into the command text: a self-selected
// variable could always claim any identity, so the role travels out of band
// through the host broker, which derived it from the facts the host reported
// for the tool call. The rewrite keeps the two session-bound flags
// (`issue create --session`, `issue close --worktree-session`) and the
// sub-agent refusal of the orchestrator-only `issue create`/`bind`.
//
// The same funnel refuses any visible mutation of the managed identity and
// transport variables before execution. An inline assignment, `export`,
// `unset`, a mutating builtin (`builtin unset`, `read`, `printf -v`, ...), or
// an `env` override could otherwise replace the broker endpoint the bridge
// injected (or strip every managed marker and become an Operator), so no part
// of the hooked command may mutate them. Redirection operands are consumed so
// `> /dev/null unset ...` cannot hide the mutation behind a filename. The guard
// scans the whole command, not only the phasegent segment, because an `unset`
// in an earlier segment and the phasegent call in a later one are separate
// commands.
//
// This is a visible-surface guard, not a sandbox: code that builds a variable
// name at runtime (`python3 -c`, `sh -c`, `eval`), a tool outside the hooked
// shell tools, and a same-uid program that talks to its own fake responder stay
// outside the observation boundary. The refusal message names the variable
// class and never echoes a value or token.

import { agentRole, isSubagentSession } from "./roles.js";
import { BROKER_ADDR_ENV, BROKER_TOKEN_ENV } from "./broker.js";
import { MANAGED_ROLE_ENV, MANAGED_SESSION_ENV } from "./managed-env.js";
import {
  maskQuoted,
  phasegentInvocation,
  scanHeredocRanges,
  shellSegments,
  shellWords,
} from "./scanner.js";
import { isWindowsHost, warn } from "./runtime.js";

// Sub-agent prohibition: the orchestrator owns issue creation and binding.
export const PHASEGENT_ISSUE_WRITE = /\bissue\s+(create|bind)\b/;
// `issue create` names its session through `--session`; `issue bind` takes no
// session flag, so only a create segment gets one injected.
export const PHASEGENT_ISSUE_CREATE = /\bissue\s+create\b/;
// `issue close` names its closer through `--worktree-session`: the parser
// rejects `--session` there with `unknown option '--session'` (exit 2,
// src/command/issue.rs), so the segment gets the flag the CLI parses.
export const PHASEGENT_ISSUE_CLOSE = /\bissue\s+close\b/;
const SESSION_FLAG = /(^|\s)--session(\s|=)/;
const WORKTREE_SESSION_FLAG = /(^|\s)--worktree-session(\s|=)/;

// Host-driven managed marker, set by the OpenCode shell tool.
export const HOST_SESSION_ENV = "OPENCODE_SESSION_ID";

// Every environment name whose visible mutation can change identity or
// transport. The legacy role variable is included even though it grants no
// identity: setting it must not make a managed invocation look ordinary.
export const MANAGED_ENV_NAMES = Object.freeze([
  MANAGED_ROLE_ENV,
  MANAGED_SESSION_ENV,
  HOST_SESSION_ENV,
  BROKER_ADDR_ENV,
  BROKER_TOKEN_ENV,
]);

const MANAGED_ENV_SET = new Set(MANAGED_ENV_NAMES);
const ENV_ASSIGNMENT_WORD = /^([A-Za-z_][A-Za-z0-9_]*)\+?=/;
const SHELL_NAME_WORD = /^[A-Za-z_][A-Za-z0-9_]*/;
const PARAMETER_EXPANSION = /^\$(?:\{)?([A-Za-z_][A-Za-z0-9_]*)/;
const ENV_CLEAR = new Set(["-i", "--ignore-environment", "--null"]);
const ENV_UNSET = new Set(["-u", "--unset"]);
const ENV_STRING = new Set(["-S", "--split-string"]);
const COMMAND_KEYWORDS = new Set(["if", "then", "else", "elif", "while", "until", "do", "!", "{", "time"]);

// The session-bearing flag of a segment: `issue create` carries `--session`,
// `issue close` carries `--worktree-session`; every other segment stays
// untouched (`issue bind` takes no session flag at all).
function sessionFlagFor(tail) {
  if (PHASEGENT_ISSUE_CREATE.test(tail)) return { name: "--session", present: SESSION_FLAG };
  if (PHASEGENT_ISSUE_CLOSE.test(tail)) {
    return { name: "--worktree-session", present: WORKTREE_SESSION_FLAG };
  }
  return null;
}

const SUBAGENT_REFUSAL =
  "echo \"phasegent: sub-agent sessions cannot run 'issue create|bind'; ask the orchestrator\" >&2; false";

// Refusal shape shared with the sub-agent guard: print one bounded message to
// stderr and fail, so no mutated command reaches the shell.
const MANAGED_ENV_REFUSAL =
  "echo \"phasegent: refusing this command because it assigns, exports, unsets, or clears a managed phasegent identity/transport environment variable; re-run without modifying those variables\" >&2; false";

function isRedirectionWord(word) {
  return /^\d*(?:>>?|<<?)/.test(word) || /^&>>?/.test(word);
}

// A redirection whose target is a separate word. `2>&1`, `>file`, and
// `<<EOF` carry their target in the same word; the separated forms need the
// operand consumed so the next word is treated as the command, not as an
// ordinary command name.
function redirectionNeedsOperand(word) {
  return /^\d*(?:>>?|<<<?-?|<>|>\|)$/.test(word) || word === "&>" || word === "&>>";
}

function skipRedirection(words, index) {
  if (redirectionNeedsOperand(words[index].value)) {
    return index + 1 < words.length ? index + 2 : -1;
  }
  return index + 1;
}

// `timeout` option tail: returns the index of its command word, or -1 for an
// unsupported/ambiguous shape (which the guard refuses).
function skipTimeout(words, start) {
  let durationSeen = false;
  for (let index = start; index < words.length; index += 1) {
    const word = words[index].value;
    if (word === "--") return durationSeen ? index + 1 : -1;
    if (word === "-s" || word === "--signal" || word === "-k" || word === "--kill-after") {
      index += 1;
      continue;
    }
    if (
      word.startsWith("--signal=") ||
      word.startsWith("--kill-after=") ||
      word === "--preserve-status" ||
      word === "--foreground" ||
      word === "-v" ||
      word === "--verbose"
    ) {
      continue;
    }
    if (word.startsWith("-")) return -1;
    if (!durationSeen) {
      durationSeen = true;
      continue;
    }
    return index;
  }
  return -1;
}

// One destination matcher shared by every mutating operation. Bash treats a
// base name, `NAME`, an indexed destination `NAME[index]`, and an assignment
// `NAME=value`/`NAME[index]=value` as the same variable. The matcher
// normalises those shapes to the base name so `unset VAR[0]` cannot slip past
// an exact-name check.
//
// A destination that looks like a managed name but cannot be conservatively
// resolved (`VAR[` with no closing bracket, `$VAR`/`${VAR}` parameter
// expansion) is reported as unresolved and the caller refuses. Ordinary data
// (`echo`, `printf` format strings, `read -p` prompts, heredoc bodies) never
// reaches this matcher because only assignment/mutator operand positions are
// inspected.
function destinationBase(word) {
  if (typeof word !== "string" || word.length === 0) return null;
  const nameMatch = SHELL_NAME_WORD.exec(word);
  if (!nameMatch) {
    const expansion = PARAMETER_EXPANSION.exec(word);
    return expansion ? { unresolved: true, candidate: expansion[1] } : null;
  }
  const base = nameMatch[0];
  const rest = word.slice(base.length);
  if (rest === "") return { base, assignment: false };
  if (rest.startsWith("=") || rest.startsWith("+=")) {
    return { base, assignment: true };
  }
  if (rest.startsWith("[")) {
    const close = rest.indexOf("]");
    if (close > 0) {
      const after = rest.slice(close + 1);
      if (after === "") return { base, assignment: false };
      if (after.startsWith("=") || after.startsWith("+=")) {
        return { base, assignment: true };
      }
    }
  }
  return { unresolved: true, candidate: base };
}

// A managed destination in any of the accepted shapes, or "unresolved" for a
// managed-looking shape the matcher cannot normalise.
function destinationMutation(word, names) {
  const destination = destinationBase(word);
  if (!destination) return null;
  if (destination.unresolved) {
    return names.has(destination.candidate) ? "unresolved" : null;
  }
  return names.has(destination.base) ? destination.base : null;
}

// Command-position assignment only: `NAME=value`, `NAME+=value`,
// `NAME[index]=value`, never a bare command word.
function assignmentMutation(word, names) {
  const destination = destinationBase(word);
  if (!destination) return null;
  if (destination.unresolved) {
    return names.has(destination.candidate) ? "unresolved" : null;
  }
  if (!destination.assignment) return null;
  return names.has(destination.base) ? destination.base : null;
}

// `export`/`unset`/`declare` style operands: any managed bare name, indexed
// destination, or assignment among the remaining words is a mutation.
function scanNames(words, start, names) {
  for (let index = start; index < words.length; index += 1) {
    const word = words[index].value;
    if (word === "--" || word.startsWith("-")) continue;
    const name = destinationMutation(word, names);
    if (name) return name;
  }
  return null;
}

// `read` assigns every operand name; `-a NAME` assigns the array. Option
// operands (prompt, delimiter, timeout) are skipped so a prompt string that
// looks like a variable name is not a false positive.
function scanRead(words, start, names) {
  let operand = false;
  for (let index = start + 1; index < words.length; index += 1) {
    const word = words[index].value;
    if (operand) {
      const name = destinationMutation(word, names);
      if (name) return name;
      continue;
    }
    if (word === "--") {
      operand = true;
      continue;
    }
    if (word.startsWith("--")) {
      if (word.startsWith("--array=")) {
        const name = destinationMutation(word.slice("--array=".length), names);
        if (name) return name;
      } else if (word === "--array") {
        const name = words[index + 1] && words[index + 1].value;
        const mutation = name && destinationMutation(name, names);
        if (mutation) return mutation;
        index += 1;
      } else if (
        ["--delimiter", "--nchars", "--prompt", "--timeout", "--delim"].includes(word)
      ) {
        index += 1;
      }
      continue;
    }
    if (word.startsWith("-") && word.length > 1) {
      const option = word[1];
      if ("dnNptu".includes(option)) {
        if (word.length === 2) index += 1;
      } else if (option === "a") {
        const name = word.length > 2 ? word.slice(2) : words[index + 1] && words[index + 1].value;
        const mutation = name && destinationMutation(name, names);
        if (mutation) return mutation;
        if (word.length === 2) index += 1;
      }
      continue;
    }
    const name = destinationMutation(word, names);
    if (name) return name;
  }
  return null;
}

// `printf -v NAME` and `mapfile`/`readarray NAME` assign a named shell
// variable. Only the assignment name is inspected; formatting text is data.
function scanPrintf(words, start, names) {
  for (let index = start + 1; index < words.length; index += 1) {
    const word = words[index].value;
    if (word === "--") break;
    if (word === "-v") {
      const name = words[index + 1] && words[index + 1].value;
      const mutation = name && destinationMutation(name, names);
      if (mutation) return mutation;
      index += 1;
    } else if (word.startsWith("-v") && word.length > 2) {
      const mutation = destinationMutation(word.slice(2), names);
      if (mutation) return mutation;
    }
  }
  return null;
}

function scanArrayAssignment(words, start, names) {
  for (let index = start + 1; index < words.length; index += 1) {
    const word = words[index].value;
    if (word === "--") {
      const name = words[index + 1] && words[index + 1].value;
      return name ? destinationMutation(name, names) : null;
    }
    if (word.startsWith("-")) {
      if (/^-[nOscCu]$/.test(word)) index += 1;
      continue;
    }
    return destinationMutation(word, names);
  }
  return null;
}

function scanGetopts(words, start, names) {
  const operands = [];
  for (let index = start + 1; index < words.length; index += 1) {
    const word = words[index].value;
    if (word.startsWith("-")) continue;
    operands.push(word);
    if (operands.length === 2) break;
  }
  return operands[1] ? destinationMutation(operands[1], names) : null;
}

// Direct shell builtins that can assign or remove a named variable from
// visible syntax. `builtin NAME ...` runs the same builtin, so it is handled
// by the same dispatch. Unknown shapes are refused by the caller.
const MUTATING_BUILTINS = new Set([
  "export",
  "declare",
  "typeset",
  "local",
  "readonly",
  "unset",
  "read",
  "printf",
  "mapfile",
  "readarray",
  "getopts",
]);

function scanMutatingBuiltin(words, start, names) {
  const name = words[start] && words[start].value;
  switch (name) {
    case "read":
      return scanRead(words, start, names);
    case "printf":
      return scanPrintf(words, start, names);
    case "mapfile":
    case "readarray":
      return scanArrayAssignment(words, start, names);
    case "getopts":
      return scanGetopts(words, start, names);
    default:
      return scanNames(words, start + 1, names);
  }
}


// `env` wrapper: clear, unset, inline assignment, or split-string recursion.
function scanEnv(words, start, names, depth) {
  for (let index = start; index < words.length; index += 1) {
    const word = words[index].value;
    if (ENV_CLEAR.has(word)) return { found: "clear", next: index };
    if (ENV_UNSET.has(word)) {
      const operand = words[index + 1] && words[index + 1].value;
      const mutation = operand && destinationMutation(operand, names);
      if (mutation) return { found: mutation, next: index };
      index += 1;
      continue;
    }
    if (word.startsWith("--unset=")) {
      const mutation = destinationMutation(word.slice("--unset=".length), names);
      if (mutation) return { found: mutation, next: index };
      continue;
    }
    if (ENV_STRING.has(word)) {
      const operand = words[index + 1] && words[index + 1].value;
      if (operand && depth < 4 && managedMutationInText(operand, names, depth + 1)) {
        return { found: "split-string", next: index };
      }
      index += 1;
      continue;
    }
    if (word === "--") return { found: null, next: index + 1 };
    const assignment = assignmentMutation(word, names);
    if (assignment) return { found: assignment, next: index };
    const destination = destinationBase(word);
    if (destination && destination.unresolved && names.has(destination.candidate)) {
      return { found: "unresolved", next: index };
    }
    if (destination && destination.assignment) {
      index += 1;
      continue;
    }
    if (word.startsWith("-")) return { found: null, next: -1 };
    // The command word: `env` ends here, and later words are its arguments.
    return { found: null, next: index };
  }
  // No command after `env`: printing the environment is not a mutation.
  return { found: null, next: words.length };
}

// Command-position walk of one segment. Assignments are only recognised where
// a simple command starts (after keywords, redirections, transparent wrappers,
// and leading assignments), so quoted arguments such as
// `--body "PHASEGENT_ROLE=x"` stay ordinary data.
function mutationInWords(words, names, depth) {
  let index = 0;
  while (index < words.length) {
    const word = words[index].value;
    if (COMMAND_KEYWORDS.has(word)) {
      index += 1;
      continue;
    }
    if (isRedirectionWord(word)) {
      const next = skipRedirection(words, index);
      if (next < 0) return "unresolved";
      index = next;
      continue;
    }
    if (word === "timeout") {
      index = skipTimeout(words, index + 1);
      if (index < 0) return "unresolved";
      continue;
    }
    if (word === "command") {
      const next = words[index + 1] && words[index + 1].value;
      if (next === "-v" || next === "-V") return null;
      index += 1;
      if (index >= words.length) return null;
      if (words[index].value.startsWith("-")) return "unresolved";
      continue;
    }
    if (word === "builtin") {
      const next = words[index + 1] && words[index + 1].value;
      if (!next) return null;
      if (next.startsWith("-")) return "unresolved";
      if (MUTATING_BUILTINS.has(next)) return scanMutatingBuiltin(words, index + 1, names);
      return null;
    }
    if (word === "env") {
      const result = scanEnv(words, index + 1, names, depth);
      if (result.found) return result.found;
      if (result.next < 0) return "unresolved";
      index = result.next;
      continue;
    }
    const assignment = assignmentMutation(word, names);
    if (assignment) return assignment;
    const destination = destinationBase(word);
    if (destination && destination.unresolved && names.has(destination.candidate)) {
      return "unresolved";
    }
    if (destination && destination.assignment) {
      index += 1;
      continue;
    }
    if (MUTATING_BUILTINS.has(word)) {
      return scanMutatingBuiltin(words, index, names);
    }
    // The ordinary command word: later words are its arguments.
    return null;
  }
  return null;
}

// Whether the whole command visibly mutates any managed variable. Quoted runs
// and heredoc data are not syntax; an unterminated quote is unresolvable and
// therefore refused.
function managedMutationInText(command, names, depth) {
  if (depth > 4) return true;
  if (!maskQuoted(command).balanced) return true;
  const heredoc = scanHeredocRanges(command);
  const opaque = (position) =>
    heredoc.some((range) => position >= range.start && position < range.end);
  for (const segment of shellSegments(command)) {
    const words = shellWords(segment.text).filter(
      (word) => !opaque(segment.start + word.start),
    );
    if (mutationInWords(words, names, depth)) return true;
  }
  return false;
}

// Whether a segment uses a transparent wrapper whose option shape cannot be
// resolved. Such a wrapper is refused (it may execute the CLI), except for the
// `command -v`/`-V` lookup shapes which execute nothing.
function hasUnresolvedWrapper(command) {
  for (const segment of shellSegments(command)) {
    const words = shellWords(segment.text);
    let index = 0;
    while (index < words.length) {
      const word = words[index].value;
      if (COMMAND_KEYWORDS.has(word)) {
        index += 1;
        continue;
      }
      if (isRedirectionWord(word)) {
        const next = skipRedirection(words, index);
        if (next < 0) return true;
        index = next;
        continue;
      }
      if (ENV_ASSIGNMENT_WORD.test(word)) {
        index += 1;
        continue;
      }
      if (word === "timeout") {
        index = skipTimeout(words, index + 1);
        if (index < 0) return true;
        continue;
      }
      if (word === "command") {
        const next = words[index + 1] && words[index + 1].value;
        if (next === "-v" || next === "-V") break;
        if (next && next.startsWith("-")) return true;
        index += 1;
        continue;
      }
      if (word === "env") {
        const result = scanEnv(words, index + 1, new Set(), 0);
        if (result.next < 0) return true;
        index = result.next;
        continue;
      }
      break;
    }
  }
  return false;
}

export function rewritePhasegentCommand(command, sessionId, event, options) {
  if (typeof command !== "string" || !command.includes("phasegent")) return command;
  const subagent = isSubagentSession(event);
  const windows =
    options && typeof options.windows === "boolean" ? options.windows : isWindowsHost();
  const source = command;
  const entries = shellSegments(source)
    .map((segment) => ({ segment, invocation: phasegentInvocation(segment) }))
    .filter((entry) => entry.invocation);
  if (entries.length === 0) {
    if (managedMutationInText(source, MANAGED_ENV_SET, 0)) {
      warn("phasegent: refused a command that mutates managed identity environment variables");
      return MANAGED_ENV_REFUSAL;
    }
    if (hasUnresolvedWrapper(source)) {
      warn("phasegent: refused a command with an unresolved wrapper around phasegent");
      return MANAGED_ENV_REFUSAL;
    }
    return source;
  }
  if (subagent && entries.some(({ invocation }) => PHASEGENT_ISSUE_WRITE.test(invocation.tail))) {
    warn("phasegent: sub-agent session refused 'issue create|bind' (orchestrator-only)");
    return SUBAGENT_REFUSAL;
  }
  if (managedMutationInText(source, MANAGED_ENV_SET, 0)) {
    warn("phasegent: refused a command that mutates managed identity environment variables");
    return MANAGED_ENV_REFUSAL;
  }
  let result = source;
  // Insert from the last invocation backwards so earlier offsets stay valid.
  for (let index = entries.length - 1; index >= 0; index -= 1) {
    const { invocation } = entries[index];
    // An unterminated quote leaves no injection point that is provably outside
    // the value: that segment stays byte-for-byte.
    if (!invocation.balanced) continue;
    // A wrapped invocation is a POSIX transparent-wrapper shape; on Windows the
    // segment has no supported interpretation, so it stays untouched rather
    // than nesting a PowerShell scope inside a foreign wrapper (issue #917 P1).
    if (windows && invocation.wrapped) continue;
    const tailFlags = [];
    const sessionFlag = sessionId ? sessionFlagFor(invocation.tail) : null;
    if (sessionFlag && !sessionFlag.present.test(invocation.tail)) {
      tailFlags.push(`${sessionFlag.name} ${sessionId}`);
    }
    const tail = tailFlags.length > 0 ? ` ${tailFlags.join(" ")}` : "";
    if (tail) {
      result = `${result.slice(0, invocation.segmentEnd)}${tail}${result.slice(invocation.segmentEnd)}`;
    }
  }
  return result;
}

// The agent role the host reported for the current tool call. The role is a
// host fact, never a command-text claim, and it is only used for the
// sub-agent refusal.
export function sessionAgentRole(event) {
  return agentRole(event);
}
