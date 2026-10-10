import { readFile } from "node:fs/promises";
import { join, resolve } from "node:path";

function fail(message) {
  throw new Error(message);
}

function parseFrontmatter(source, file) {
  const match = source.match(/^---\r?\n([\s\S]*?)\r?\n---(?:\r?\n|$)/);
  if (!match) fail(`${file}: missing YAML frontmatter`);
  const parsed = Bun.YAML.parse(match[1]);
  if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) {
    fail(`${file}: frontmatter must be a YAML mapping`);
  }
  return parsed;
}

function questionRules(agent, file, { required = true } = {}) {
  if (Object.hasOwn(agent, "permission")) {
    fail(`${file}: unsupported singular permission key; use permissions`);
  }
  if (!Object.hasOwn(agent, "permissions")) {
    if (required) fail(`${file}: permissions must be an ordered list`);
    return [];
  }
  if (!Array.isArray(agent.permissions)) {
    fail(`${file}: permissions must be an ordered list`);
  }
  for (const [index, rule] of agent.permissions.entries()) {
    if (
      !rule ||
      typeof rule !== "object" ||
      Array.isArray(rule) ||
      typeof rule.action !== "string" ||
      typeof rule.resource !== "string" ||
      typeof rule.effect !== "string"
    ) {
      fail(`${file}: permission rule ${index + 1} must declare action, resource, and effect`);
    }
  }
  return agent.permissions.filter(
    (rule) =>
      (rule.action === "question" || rule.action === "*") &&
      rule.resource === "*",
  );
}

async function readAgent(agentDir, name) {
  const file = join(agentDir, `${name}.md`);
  return { file, frontmatter: parseFrontmatter(await readFile(file, "utf8"), file) };
}

async function main() {
  const args = process.argv.slice(2);
  if (args.length !== 1 || !args[0]) {
    fail("usage: bun scripts/check-agent-question-permissions.mjs <agent-dir>");
  }
  const agentDir = resolve(args[0]);

  for (const name of ["executor", "reviewer", "explore"]) {
    const { file, frontmatter } = await readAgent(agentDir, name);
    const rules = questionRules(frontmatter, file);
    const last = rules.at(-1);
    if (
      !last ||
      last.action !== "question" ||
      last.resource !== "*" ||
      last.effect !== "deny"
    ) {
      fail(`${file}: last matching question rule must be {action: question, resource: '*', effect: deny}`);
    }
  }

  const { file, frontmatter } = await readAgent(agentDir, "orchestrator");
  if (frontmatter.mode !== "primary") {
    fail(`${file}: orchestrator mode must remain primary`);
  }
  const primaryQuestionRules = questionRules(frontmatter, file, { required: false });
  if (primaryQuestionRules.at(-1)?.effect === "deny") {
    fail(`${file}: question must not be denied for the primary`);
  }

  console.log(`question permissions verified in ${agentDir}`);
}

main().catch((error) => {
  console.error(`agent question permission check failed: ${error.message}`);
  process.exitCode = 1;
});
