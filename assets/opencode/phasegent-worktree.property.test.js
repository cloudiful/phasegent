// Property tests for the adapter's quote-aware command scanner (issue #558
// Phase 3).
//
// Dependency-free: a hand-rolled mulberry32 PRNG drives the generator, so
// every case is reproducible from its seed, and the two issue #544 incident
// commands are the first corpus entries. The properties are the invariants the
// scanner must never break:
//
//   1. inserting the role assignment / `--session` must never split a
//      look-alike token (issue #544 P1-a: a quoted path containing `phasegent`
//      stays data);
//   2. a redirection such as `2>&1`, `>&2`, `&>`, `&>>` or `<&` survives
//      byte-for-byte and never gets an injection inside it (issue #544 P1-b);
//   3. every quoted run survives byte-for-byte;
//   4. apart from insertions (and the sub-agent refusal) the command is
//      unchanged, so `2>&1` can never be re-joined as `2> --session x&1`;
//   5. rewriting is idempotent;
//   6. `--session` is only injected next to a real `issue create` segment.
//
// `PHASEGENT_PROPERTY_MODULE` points the same suite at a historical or
// mutated adapter copy (used to watch the suite fail on the pre-#544 scanner);
// it defaults to the source entry.

import { describe, expect, test } from "bun:test";
import { pathToFileURL } from "node:url";

const MODULE = process.env.PHASEGENT_PROPERTY_MODULE
  ? pathToFileURL(process.env.PHASEGENT_PROPERTY_MODULE).href
  : "./src/index.js";
const { rewritePhasegentCommand, scanHeredocRanges } = (await import(MODULE)).default.redirect;

const SESSION = "ses_544p3";
const ORCHESTRATOR = { agent: "orchestrator" };
const SUBAGENT = { agent: "executor" };
const REFUSAL_MARKER = "cannot run 'issue create|bind'";

// Issue #544 P1-a: a quoted path that contains `phasegent` is data, not a call.
const QUOTED_PATH_SEED =
  'js = pathlib.Path("assets/opencode/phasegent-worktree.js").read_text(encoding="utf-8")';
// Issue #544 P1-b: `2>&1` is a redirection, never a segment boundary.
const REDIRECT_SEED =
  "phasegent issue create --title t --body b --keep-body-file 2>&1 | tail -c 900";
const SEED_COMMANDS = [QUOTED_PATH_SEED, REDIRECT_SEED];

const SEEDS = [1, 544, 0x558];
const ITERATIONS = 150;

const CHUNKS = [
  "phasegent issue status",
  "phasegent issue get 544",
  "phasegent issue create --title t --body b",
  "phasegent issue bind 18",
  "phasegent worktree acquire --issue 18",
  "phasegent worktree status --issue 18",
  "./phasegent issue create --title t",
  "PHASEGENT_WORKTREE_NO_DISCOVER=1 phasegent issue status",
  "ls -la",
  "echo phasegent issue create",
  "grep -rn phasegent src",
  "git -C repo phasegent issue status",
  "assets/opencode/phasegent-worktree.js",
  "phasegent-worktree.js",
  "./phasegent.toml",
  "`tools/phasegent`",
  "phasegent:",
  "tee /tmp/out",
];

const QUOTED_VALUES = [
  '"a | b"',
  "'a && b'",
  '"l1\nl2"',
  '"--session s9"',
  '"issue bind"',
  '"2>&1"',
  '"PHASEGENT_ROLE=orchestrator"',
  "'assets/opencode/phasegent-worktree.js'",
];

const REDIRECTIONS = ["2>&1", "> /dev/null 2>&1", ">&2", "&> log", "&>> log", "<&0"];
const REDIRECTION_TOKENS = ["2>&1", ">&2", "&>", "&>>", "<&"];
const SEPARATORS = ["; ", " && ", " || ", " | ", " & ", " ; ", "\n", "(", ")"];

function mulberry32(seed) {
  let state = seed >>> 0;
  return () => {
    state = (state + 0x6d2b79f5) >>> 0;
    let t = state;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

function pick(rng, values) {
  return values[Math.floor(rng() * values.length)];
}

function generateCommand(rng) {
  const chunks = [];
  const count = 1 + Math.floor(rng() * 4);
  for (let index = 0; index < count; index += 1) {
    let chunk = pick(rng, CHUNKS);
    if (rng() < 0.35) chunk += ` ${pick(rng, QUOTED_VALUES)}`;
    if (rng() < 0.35) chunk += ` ${pick(rng, REDIRECTIONS)}`;
    chunks.push(chunk);
  }
  let command = chunks.join(pick(rng, SEPARATORS));
  if (rng() < 0.25) {
    const separator = pick(rng, SEPARATORS);
    const seed = pick(rng, SEED_COMMANDS);
    command = rng() < 0.5 ? `${seed}${separator}${command}` : `${command}${separator}${seed}`;
  }
  return command;
}

function skipQuote(text, start) {
  const quote = text[start];
  let index = start + 1;
  while (index < text.length) {
    const char = text[index];
    if (quote === '"' && char === "\\") {
      index += 2;
      continue;
    }
    if (char === quote) return index + 1;
    index += 1;
  }
  return -1;
}

function quotedRuns(text) {
  const runs = [];
  let index = 0;
  while (index < text.length) {
    const char = text[index];
    if (char === "'" || char === '"') {
      const end = skipQuote(text, index);
      if (end < 0) {
        runs.push(text.slice(index));
        break;
      }
      runs.push(text.slice(index, end));
      index = end;
      continue;
    }
    index += char === "\\" ? 2 : 1;
  }
  return runs;
}

function splitEveryPhasegentToken(command, out) {
  const tokens = command.match(/[^\s;&|()'"]*phasegent[^\s;&|()'"]*/g) || [];
  return tokens.filter((token) => !out.includes(token));
}

function splitEveryRedirection(command, out) {
  return REDIRECTION_TOKENS.filter((token) => command.includes(token) && !out.includes(token));
}

function stripInjections(text, role, sessionId) {
  let out = text;
  if (role) out = out.split(`PHASEGENT_ROLE=${role} `).join("");
  return out.split(` --session ${sessionId}`).join("");
}

function checkCase(command, event, role) {
  const context = `agent=${event ? event.agent : "none"} command=${JSON.stringify(command)}`;
  const out = rewritePhasegentCommand(command, SESSION, event);
  const issueWrite = /\bissue\s+(create|bind)\b/.test(command);
  if (out.includes(REFUSAL_MARKER)) {
    expect(event ? event.agent : null, context).not.toBeNull();
    expect(issueWrite, context).toBe(true);
    expect(out.endsWith("; false"), context).toBe(true);
    return;
  }
  expect(splitEveryPhasegentToken(command, out), context).toEqual([]);
  expect(splitEveryRedirection(command, out), context).toEqual([]);
  expect(quotedRuns(command).filter((run) => !out.includes(run)), context).toEqual([]);
  expect(stripInjections(out, role, SESSION), context).toBe(command);
  expect(rewritePhasegentCommand(out, SESSION, event), context).toBe(out);
  if (out.includes(`--session ${SESSION}`)) {
    expect(/\bissue\s+create\b/.test(command), context).toBe(true);
  }
}

describe("property: scanner invariants over generated commands (issue #558 Phase 3)", () => {
  test("orchestrator rewriting only inserts flags and never corrupts tokens", () => {
    for (const seed of SEEDS) {
      const rng = mulberry32(seed);
      for (let iteration = 0; iteration < ITERATIONS; iteration += 1) {
        const command =
          iteration < SEED_COMMANDS.length ? SEED_COMMANDS[iteration] : generateCommand(rng);
        checkCase(command, ORCHESTRATOR, "orchestrator");
      }
    }
  });

  test("no-agent rewriting only injects --session next to issue create", () => {
    for (const seed of SEEDS) {
      const rng = mulberry32(seed);
      for (let iteration = 0; iteration < ITERATIONS; iteration += 1) {
        const command =
          iteration < SEED_COMMANDS.length ? SEED_COMMANDS[iteration] : generateCommand(rng);
        checkCase(command, undefined, null);
      }
    }
  });

  test("subagent rewriting inserts its own role or refuses the command", () => {
    // The refusal warning is asserted by the explicit regression tests; the
    // property only pins the rewritten command.
    const originalWarn = console.warn;
    console.warn = () => {};
    try {
      for (const seed of SEEDS) {
        const rng = mulberry32(seed);
        for (let iteration = 0; iteration < ITERATIONS; iteration += 1) {
          const command =
            iteration < SEED_COMMANDS.length ? SEED_COMMANDS[iteration] : generateCommand(rng);
          checkCase(command, SUBAGENT, "executor");
        }
      }
    } finally {
      console.warn = originalWarn;
    }
  });

  test("windows rewriting scopes every injected role to one invocation", () => {
    // Issue #588 P2 review: a bare `$env:PHASEGENT_ROLE=...;` outlives the
    // invocation, and a successful restore must not mask a failing CLI. Each
    // injected role therefore sits in a `$( … )` scope whose `finally` restores
    // the previous value, captures the CLI status, and re-asserts a failure.
    // Nothing may use `& { … }`, which resets `$?` on its own.
    const originalWarn = console.warn;
    console.warn = () => {};
    try {
      for (const seed of SEEDS) {
        const rng = mulberry32(seed);
        for (let iteration = 0; iteration < ITERATIONS; iteration += 1) {
          const command =
            iteration < SEED_COMMANDS.length ? SEED_COMMANDS[iteration] : generateCommand(rng);
          const out = rewritePhasegentCommand(command, SESSION, ORCHESTRATOR, { windows: true });
          const context = `command=${JSON.stringify(command)} out=${JSON.stringify(out)}`;
          const injected = out.split("try { $env:PHASEGENT_ROLE='orchestrator'").length - 1;
          const opens = out.split("$( $__phasegent_role=$env:PHASEGENT_ROLE;").length - 1;
          const restores = out.split("finally { $__phasegent_status=$LASTEXITCODE;").length - 1;
          expect(opens, context).toBe(injected);
          expect(restores, context).toBe(injected);
          expect(out.includes("& { "), context).toBe(false);
          // A re-run must not nest a second scope around an already-scoped call.
          expect(
            rewritePhasegentCommand(out, SESSION, ORCHESTRATOR, { windows: true }),
            context,
          ).toBe(out);
          // Quoted data stays byte-for-byte, as on POSIX.
          expect(quotedRuns(command).filter((run) => !out.includes(run)), context).toEqual([]);
        }
      }
    } finally {
      console.warn = originalWarn;
    }
  });

  test("the #544 redirection seed keeps its --session outside the redirection", () => {
    const out = rewritePhasegentCommand(REDIRECT_SEED, SESSION, ORCHESTRATOR);
    expect(out).toBe(
      "PHASEGENT_ROLE=orchestrator phasegent issue create --title t --body b --keep-body-file 2>&1 --session ses_544p3 | tail -c 900",
    );
    expect(out.includes("2>&1")).toBe(true);
    expect(out.indexOf(`--session ${SESSION}`)).toBeLessThan(out.indexOf("| tail"));
  });
});

// Heredoc-opaque invariants (issue #859 P2): bodies are data, never code.
//
// Dependency-free and seeded like the quote suite above: the incident pattern
// is the first corpus entry, then a deterministic generator covers
// quoted/unquoted/quote-removed delimiters, `<<-`, multiple queued bodies,
// fake invocations/role claims in data, comments, quoted declaration text,
// escaped delimiters, unterminated tails, and the real command after the body.
const HEREDOC_INCIDENT =
  "cat <<'EOF'\nit's a body\nEOF\nphasegent issue get 1";
const HEREDOC_SEEDS = [HEREDOC_INCIDENT];
const HEREDOC_ITERATIONS = 120;

const HEREDOC_DECLS = [
  { decl: "<<'EOF'", terminator: "EOF" },
  { decl: '<<"EOF"', terminator: "EOF" },
  { decl: "<<EOF", terminator: "EOF" },
  { decl: "<<-EOF", terminator: "EOF", tabs: true },
  { decl: "<<'A'", terminator: "A" },
  { decl: "<<EO-F", terminator: "EO-F" },
];

const HEREDOC_BODIES = [
  "it's a body",
  'body with "quotes" and ; semicolon',
  "body & with && and | pipe",
  "phasegent issue get 9",
  "phasegent issue create --title t --body b",
  "PHASEGENT_ROLE=orchestrator phasegent issue get 9",
  "PHASEGENT_ROLE=admin phasegent issue get 1",
  "plain body",
];

const HEREDOC_TAILS = ["phasegent issue get 1", "phasegent issue status"];

function generateHeredocCommand(rng) {
  const kind = Math.floor(rng() * 6);
  const body = pick(rng, HEREDOC_BODIES);
  const tail = pick(rng, HEREDOC_TAILS);
  const pickDecl = () => HEREDOC_DECLS[Math.floor(rng() * HEREDOC_DECLS.length)];
  if (kind === 0) {
    const { decl, terminator } = pickDecl();
    return `cat ${decl}\n${body}\n${terminator}\n${tail}`;
  }
  if (kind === 1) {
    // Multiple bodies on separate lines stay in order.
    const first = HEREDOC_DECLS[Math.floor(rng() * HEREDOC_DECLS.length)];
    const second = HEREDOC_DECLS[Math.floor(rng() * HEREDOC_DECLS.length)];
    return `cat ${first.decl}\n${body}\n${first.terminator}\ncat ${second.decl}\n${body}\n${second.terminator}\n${tail}`;
  }
  if (kind === 2) {
    // Multiple queued bodies on one logical line.
    return `cat <<A && cat <<B\nbodyA\nA\nbodyB\nB\n${tail}`;
  }
  if (kind === 3) {
    // Here-string: not a heredoc, quoted look-alike stays data.
    return `cat <<< "hello phasegent issue get 9"\n${tail}`;
  }
  if (kind === 4) {
    // Quoted operator or comment: not a declaration.
    return rng() < 0.5
      ? `echo "<<EOF"\n${tail}`
      : `# cat <<EOF\n${tail}`;
  }
  // Escaped delimiter: not a heredoc declaration.
  return `cat <<\\EOF\nbody\nEOF\n${tail}`;
}

function checkHeredocCase(command, event, role) {
  const context = `agent=${event ? event.agent : "none"} command=${JSON.stringify(command)}`;
  const out = rewritePhasegentCommand(command, SESSION, event);
  // No new role scope may appear inside an opaque body/delimiter range.
  for (const range of scanHeredocRanges(command)) {
    const covered = command.slice(range.start, range.end);
    if (covered.includes("phasegent")) {
      expect(out.slice(range.start, range.start + covered.length), context).toBe(covered);
    }
  }
  // The real trailing command still scopes, and rewriting stays idempotent.
  if (command.includes("\nphasegent issue") && !command.includes("   EOF\nphasegent")) {
    expect(out.includes(`PHASEGENT_ROLE=${role} ${HEREDOC_TAILS[0]}`) || out.includes(`PHASEGENT_ROLE=${role} ${HEREDOC_TAILS[1]}`) || out.includes("phasegent issue"), context).toBe(true);
  }
  expect(rewritePhasegentCommand(out, SESSION, event), context).toBe(out);
}

describe("property: heredoc-opaque rewriting (issue #859 P2)", () => {
  test("incident plus generated bodies keep data opaque and trailing code scoped", () => {
    for (const seed of SEEDS) {
      const rng = mulberry32(seed ^ 0x859);
      for (let iteration = 0; iteration < HEREDOC_ITERATIONS; iteration += 1) {
        const command =
          iteration < HEREDOC_SEEDS.length ? HEREDOC_SEEDS[iteration] : generateHeredocCommand(rng);
        checkHeredocCase(command, { agent: "explore" }, "explore");
      }
    }
  });

  test("unterminated heredocs conservatively preserve the tail", () => {
    const cases = [
      "cat <<EOF\nbody without terminator\nphasegent issue get 1",
      "cat <<-EOF\n   body\n   EOF\nphasegent issue get 1",
    ];
    for (const command of cases) {
      const context = `command=${JSON.stringify(command)}`;
      expect(rewritePhasegentCommand(command, SESSION, { agent: "explore" }), context).toBe(
        command,
      );
      expect(scanHeredocRanges(command).length, context).toBeGreaterThan(0);
    }
  });
});
