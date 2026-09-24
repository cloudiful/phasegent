// Shared runtime helpers: error text, console warnings, and the `phasegent`
// CLI probes (Bun shell).
//
// OpenCode v2 has no structured warning field on the plugin context, so every
// degradation is reported on the host console and never throws into a tool
// call.

export function errorText(error) {
  return String(error && error.message ? error.message : error);
}

export function warn(message) {
  try {
    console.warn(message);
  } catch (_) {
    // ignore: a broken console must not break path redirection
  }
}

export async function safeText(command) {
  try {
    const text = (await command.text()).trim();
    return { ok: true, value: text };
  } catch (error) {
    return { ok: false, error: errorText(error) };
  }
}

export function phasegentCallsDisabled() {
  try {
    return Boolean(process && process.env && process.env.PHASEGENT_WORKTREE_NO_DISCOVER === "1");
  } catch (_) {
    return false;
  }
}

// The host platform decides how a role is expressed in a shell command: POSIX
// shells take the `NAME=value` prefix, PowerShell needs `$env:NAME='value'`.
export function isWindowsHost() {
  try {
    return typeof process !== "undefined" && process.platform === "win32";
  } catch (_) {
    return false;
  }
}

// `args` is spread into separate argv entries by Bun's shell interpolation, and
// `env` (when given) is layered over the inherited environment for this probe
// only, so a role never leaks into the host process.
export function phasegentCommand(args, cwd, env) {
  const command = Bun.$`phasegent ${args}`;
  const scoped = cwd ? command.cwd(cwd) : command;
  return (env ? scoped.env({ ...process.env, ...env }) : scoped).quiet();
}

export function locationDirectory(context) {
  const location = context && context.location;
  return location && typeof location.directory === "string" ? location.directory : null;
}
