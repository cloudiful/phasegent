// Pure path redirect helpers (issue #440; split by issue #541).
//
// Only relative values are rewritten; everything absolute (POSIX, Windows drive
// or UNC) is returned verbatim so an explicit escape is never silently
// retargeted and the external_directory check still sees the path the model
// asked for. v2 renamed the file tools' `filePath` to `path` and the shell tool
// from `bash` to `shell` (packages/core/src/tool/plugin/{read,write,edit}.ts,
// tool/shell.ts:22). Shell command rewriting (issue #541) lives in the hook;
// these helpers only retarget filesystem paths.

export function isAbsolutePath(value) {
  if (typeof value !== "string" || value.length === 0) return false;
  if (value.startsWith("/")) return true;
  if (/^[A-Za-z]:[\\/]/.test(value)) return true;
  return value.startsWith("\\\\");
}

export function redirectPathValue(workdir, value) {
  if (typeof workdir !== "string" || workdir.length === 0) return value;
  if (typeof value !== "string" || value.length === 0) return value;
  if (isAbsolutePath(value)) return value;
  return `${workdir.replace(/[\\/]+$/, "")}/${value.replace(/^[\\/]+/, "")}`;
}

const PATH_ARG_KEYS = {
  read: ["path"],
  write: ["path"],
  edit: ["path"],
  glob: ["path"],
  grep: ["path"],
};

export const SEARCH_TOOLS = ["glob", "grep"];
export const SHELL_TOOLS = ["shell", "bash"];

export function redirectPaths(tool, workdir, args) {
  if (!args || typeof args !== "object") return args;
  if (typeof workdir !== "string" || workdir.length === 0) return args;
  const redirected = { ...args };
  const keys = PATH_ARG_KEYS[tool];
  if (SEARCH_TOOLS.includes(tool)) {
    const current = redirected.path;
    if (typeof current !== "string" || current.length === 0) {
      redirected.path = workdir;
    } else {
      redirected.path = redirectPathValue(workdir, current);
    }
  } else if (keys) {
    for (const key of keys) {
      if (typeof redirected[key] === "string") {
        redirected[key] = redirectPathValue(workdir, redirected[key]);
      }
    }
  }
  if (SHELL_TOOLS.includes(tool)) {
    const current = redirected.workdir;
    if (typeof current === "string" && current.length > 0) {
      redirected.workdir = redirectPathValue(workdir, current);
    } else {
      // Bare shell: the shell would otherwise default to the stale session cwd.
      redirected.workdir = workdir;
    }
  }
  return redirected;
}
