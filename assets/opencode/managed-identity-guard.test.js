// End-to-end probe for the visible managed-variable guard (issue #22 P3).
//
// The generated plugin hook is the only thing that sees a command before the
// host shell runs it. These tests push a real command through that hook and
// then execute the resulting shell text: a visible inline broker override or
// marker removal must be refused before the CLI is reached, while a legitimate
// command still reaches a real loopback broker and succeeds.
//
// The probe is skipped when the debug binary has not been built, so a bare
// `bun test` stays usable; the Rust test gate builds it first.

import { describe, expect, test } from "bun:test";
import { spawn } from "node:child_process";
import { existsSync } from "node:fs";
import net from "node:net";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import plugin from "./phasegent-worktree.js";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");
const cli = process.env.PHASEGENT_TEST_BIN || join(repoRoot, "target", "debug", "phasegent");
const hasCli = existsSync(cli);
const cliTest = hasCli ? test : test.skip;

function cleanEnv(extra) {
  const env = {};
  for (const [name, value] of Object.entries(process.env)) {
    if (name.startsWith("PHASEGENT_") || name === "OPENCODE_SESSION_ID") continue;
    env[name] = value;
  }
  return { ...env, ...extra };
}

function runShellIn(shell, command, env) {
  return new Promise((resolve) => {
    const child = spawn(shell, ["-c", command], {
      env: cleanEnv(env),
      stdio: ["ignore", "pipe", "pipe"],
    });
    let stdout = "";
    let stderr = "";
    child.stdout.on("data", (chunk) => {
      stdout += chunk.toString("utf8");
    });
    child.stderr.on("data", (chunk) => {
      stderr += chunk.toString("utf8");
    });
    const timer = setTimeout(() => child.kill("SIGKILL"), 15000);
    child.on("close", (status, signal) => {
      clearTimeout(timer);
      resolve({ status, signal, stdout, stderr });
    });
  });
}

function runShell(command, env) {
  return runShellIn("/bin/sh", command, env);
}

function runBash(command, env) {
  return runShellIn("/bin/bash", command, env);
}

// A loopback responder that answers one identify request. `connections` counts
// every accepted socket so a refusal can prove the CLI never connected.
function startResponder(reply) {
  const state = { connections: 0 };
  const server = net.createServer((socket) => {
    state.connections += 1;
    let buffer = "";
    socket.on("data", (chunk) => {
      buffer += chunk.toString("utf8");
      if (!buffer.includes("\n")) return;
      const body = buffer.slice(0, buffer.indexOf("\n"));
      let request = {};
      try {
        request = JSON.parse(body);
      } catch (_) {
        request = {};
      }
      socket.end(`${JSON.stringify(reply(request))}\n`);
    });
  });
  return new Promise((resolve) => {
    server.listen(0, "127.0.0.1", () => {
      resolve({
        state,
        addr: `127.0.0.1:${server.address().port}`,
        close: () => new Promise((done) => server.close(done)),
      });
    });
  });
}

async function applyHook(command, agent = "executor") {
  const hook = plugin.redirect.createRedirectHook();
  const event = {
    tool: "bash",
    sessionID: "ses-probe",
    agent,
    input: { command },
  };
  await hook(event);
  return event.input.command;
}

describe("actual plugin-hook to CLI managed-identity probe", () => {
  cliTest("refuses a visible inline fake-broker override before the CLI runs", async () => {
    const responder = await startResponder((request) => ({
      v: 1,
      ok: true,
      role: "orchestrator",
      session: request.session,
    }));
    try {
      const command = `${cli} --help issue status`;
      const forged = `PHASEGENT_BROKER_ADDR=${responder.addr} PHASEGENT_BROKER_TOKEN=fake ${command}`;
      const rewritten = await applyHook(forged);
      expect(rewritten).toContain("refusing this command");
      expect(rewritten).not.toContain(responder.addr);
      expect(rewritten).not.toContain("--help issue status");
      const result = await runShell(rewritten);
      expect(result.status).not.toBe(0);
      expect(result.stderr).toContain("refusing this command");
      expect(responder.state.connections).toBe(0);
    } finally {
      await responder.close();
    }
  });

  cliTest("refuses visible marker stripping before the CLI runs", async () => {
    const command = `${cli} --help issue status`;
    const stripped = `env -u PHASEGENT_SESSION_ID -u OPENCODE_SESSION_ID -u PHASEGENT_BROKER_ADDR -u PHASEGENT_BROKER_TOKEN ${command}`;
    const rewritten = await applyHook(stripped);
    expect(rewritten).toContain("refusing this command");
    const result = await runShell(rewritten);
    expect(result.status).not.toBe(0);
    expect(result.stderr).toContain("refusing this command");
  });

  cliTest("refuses builtin and separated-redirection marker stripping before the CLI runs", async () => {
    const markers =
      "PHASEGENT_BROKER_ADDR PHASEGENT_BROKER_TOKEN PHASEGENT_SESSION_ID OPENCODE_SESSION_ID PHASEGENT_ROLE";
    const cases = [
      `builtin unset ${markers}; ${cli} --help admin auth setup`,
      `> /dev/null unset ${markers}; ${cli} --help admin auth setup`,
      `2>&1 unset ${markers}; ${cli} --help admin auth setup`,
      `builtin export PHASEGENT_ROLE=admin; ${cli} --help admin auth setup`,
      `builtin declare PHASEGENT_SESSION_ID=other; ${cli} --help admin auth setup`,
      `builtin read -r PHASEGENT_SESSION_ID <<< x; ${cli} --help admin auth setup`,
      `read -a PHASEGENT_ROLE <<< x; ${cli} --help admin auth setup`,
      `read --array PHASEGENT_ROLE <<< x; ${cli} --help admin auth setup`,
      `read PHASEGENT_SESSION_ID <<< x; ${cli} --help admin auth setup`,
      `printf -v PHASEGENT_SESSION_ID x; ${cli} --help admin auth setup`,
      `mapfile -t PHASEGENT_ROLE <<< x; ${cli} --help admin auth setup`,
      `getopts a PHASEGENT_SESSION_ID; ${cli} --help admin auth setup`,
    ];
    for (const command of cases) {
      const rewritten = await applyHook(command);
      expect(rewritten, command).toContain("refusing this command");
      expect(rewritten, command).not.toContain("--help admin auth setup");
      const result = await runBash(rewritten);
      expect(result.status, command).not.toBe(0);
      expect(result.stderr, command).toContain("refusing this command");
    }
  });

  cliTest("refuses indexed unset and indexed mutator destinations before the CLI runs", async () => {
    const markers = [
      "PHASEGENT_BROKER_ADDR",
      "PHASEGENT_BROKER_TOKEN",
      "PHASEGENT_SESSION_ID",
      "OPENCODE_SESSION_ID",
      "PHASEGENT_ROLE",
    ];
    const indexed = markers.map((name) => `${name}[0]`).join(" ");
    const cases = [
      `unset ${indexed}; ${cli} --help admin auth setup`,
      `unset "PHASEGENT_SESSION_ID[0]"; ${cli} --help admin auth setup`,
      `export PHASEGENT_ROLE[0]=admin; ${cli} --help admin auth setup`,
      `declare -x PHASEGENT_ROLE[0]=admin; ${cli} --help admin auth setup`,
      `read -r "PHASEGENT_SESSION_ID[0]" <<< x; ${cli} --help admin auth setup`,
      `printf -v "PHASEGENT_SESSION_ID[0]" x; ${cli} --help admin auth setup`,
      `mapfile -t "PHASEGENT_ROLE[0]" <<< x; ${cli} --help admin auth setup`,
      `getopts a "PHASEGENT_SESSION_ID[0]"; ${cli} --help admin auth setup`,
    ];
    for (const command of cases) {
      const rewritten = await applyHook(command);
      expect(rewritten, command).toContain("refusing this command");
      expect(rewritten, command).not.toContain("--help admin auth setup");
      const result = await runBash(rewritten);
      expect(result.status, command).not.toBe(0);
      expect(result.stderr, command).toContain("refusing this command");
    }
    const safe = `unset OTHER[0]; echo PHASEGENT_ROLE[0]; ${cli} --help issue status`;
    expect(await applyHook(safe)).not.toContain("refusing this command");
  });

  cliTest("a legitimate command still verifies against a real broker", async () => {
    const responder = await startResponder((request) => ({
      v: 1,
      ok: request.token === "real-token" && request.session === "ses-probe",
      role: "executor",
      session: request.session,
    }));
    try {
      // `record list` resolves the caller before any provider access; it may
      // still fail on the local store, which is not what this probe checks.
      const command = `${cli} --provider local record list 1`;
      const rewritten = await applyHook(command);
      expect(rewritten).toBe(command);
      const result = await runShell(rewritten, {
        PHASEGENT_BROKER_ADDR: responder.addr,
        PHASEGENT_BROKER_TOKEN: "real-token",
        PHASEGENT_SESSION_ID: "ses-probe",
        OPENCODE_SESSION_ID: "ses-probe",
      });
      expect(responder.state.connections).toBeGreaterThanOrEqual(1);
      expect(result.stderr).not.toContain("managed session requires a role");
    } finally {
      await responder.close();
    }
  });
});
