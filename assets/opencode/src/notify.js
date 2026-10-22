// Best-effort question-awaiting-user notification (issue #1026 P1).
//
// The adapter subscribes to the host event stream and, for each *new* local
// pending question form, invokes the existing `phasegent notify send` CLI once.
// It never answers a question, never blocks or fails the question tool, and
// never inspects credentials or local databases: delivery is one bounded,
// argv-only child process that reuses the operator's configured channel and
// inherits the environment unchanged (including any role/session markers the
// ambient caller already carries).
//
// The official v2 subscription is an AsyncIterable (`context.event.subscribe`),
// the payload lives in `event.data.form`, and a question is a `form.created`
// whose `metadata.kind` is `"question"`. Only events whose location matches this
// plugin instance are sent to, so a shared host never notifies for another
// workspace's question.

import { locationDirectory, warn } from "./runtime.js";

export const QUESTION_NOTIFY_TITLE = "OpenCode question pending";
export const QUESTION_NOTIFY_TIMEOUT_MS = 10000;
export const QUESTION_DEDUP_LIMIT = 256;

const IDENTITY = ["phasegent", "notify", "send", "--event", "blocked"];

function defaultSpawn(argv, options) {
  return Bun.spawn(argv, options);
}

function questionBody(sessionID, fieldCount) {
  const suffix = fieldCount === 1 ? "1 field" : `${fieldCount} fields`;
  return `Session ${sessionID} has a pending question (${suffix}). Answer it in OpenCode.`;
}

function rememberForm(seen, id) {
  seen.add(id);
  while (seen.size > QUESTION_DEDUP_LIMIT) {
    const oldest = seen.values().next().value;
    seen.delete(oldest);
  }
}

function isQuestionForm(form) {
  if (!form || typeof form !== "object") return false;
  if (typeof form.id !== "string" || form.id.length === 0) return false;
  const metadata = form.metadata && typeof form.metadata === "object" ? form.metadata : null;
  return Boolean(metadata) && metadata.kind === "question";
}

async function readFormSessionLocation(context, sessionID) {
  const get = context && context.session && context.session.get;
  if (typeof get !== "function") return null;
  if (typeof sessionID !== "string" || sessionID.length === 0) return null;
  try {
    const result = await get({ sessionID });
    if (!result || typeof result !== "object") return null;
    const location = result.location && typeof result.location === "object" ? result.location : null;
    const directory = location && typeof location.directory === "string" ? location.directory : null;
    if (!directory) return null;
    const workspaceID = location && typeof location.workspaceID === "string" ? location.workspaceID : null;
    return { directory, workspaceID };
  } catch (_) {
    return null;
  }
}

// The form belongs to this plugin instance only when its directory equals the
// plugin's own location; a missing/mismatched/unresolvable location skips (with a
// safe warning when unknown), so a shared host never notifies cross-location.
async function formBelongsToInstance(context, event, form, report) {
  const own = locationDirectory(context);
  if (!own) {
    report("phasegent: question notification skipped: plugin location unavailable");
    return false;
  }
  const eventLocation =
    event.location && typeof event.location === "object" ? event.location : null;
  let directory =
    eventLocation && typeof eventLocation.directory === "string" ? eventLocation.directory : null;
  let workspaceID =
    eventLocation && typeof eventLocation.workspaceID === "string" ? eventLocation.workspaceID : null;
  if (!directory) {
    const resolved = await readFormSessionLocation(context, form.sessionID);
    if (!resolved) {
      report("phasegent: question notification skipped: form location unknown");
      return false;
    }
    directory = resolved.directory;
    if (!workspaceID) workspaceID = resolved.workspaceID;
  }
  if (directory !== own) return false;
  const ownWorkspace =
    context.location && typeof context.location.workspaceID === "string"
      ? context.location.workspaceID
      : null;
  if (workspaceID && ownWorkspace && workspaceID !== ownWorkspace) return false;
  return true;
}

// `createQuestionNotifier` starts one abortable background subscriber and returns
// a disposer. `dependencies.spawn`/`dependencies.warn` are injectable so the
// delivery path is deterministic in tests; production uses `Bun.spawn` and the
// console warning channel.
export function createQuestionNotifier(context, dependencies = {}) {
  const spawn = typeof dependencies.spawn === "function" ? dependencies.spawn : defaultSpawn;
  const report = typeof dependencies.warn === "function" ? dependencies.warn : warn;
  const subscribe = context && context.event && context.event.subscribe;
  if (typeof subscribe !== "function") {
    report("phasegent: host exposes no event subscription; question notifications are disabled");
    return () => {};
  }

  const controller = new AbortController();
  const seen = new Set();
  const children = new Set();
  let disposed = false;

  const deliver = (form) => {
    const sessionID = typeof form.sessionID === "string" ? form.sessionID : "unknown";
    const fieldCount = Array.isArray(form.fields) ? form.fields.length : 0;
    const argv = [...IDENTITY, "--title", QUESTION_NOTIFY_TITLE, "--body", questionBody(sessionID, fieldCount)];
    const cwd = locationDirectory(context);
    const options = {
      cwd: cwd || undefined,
      env: process.env,
      stdin: "ignore",
      stdout: "ignore",
      stderr: "ignore",
      timeout: QUESTION_NOTIFY_TIMEOUT_MS,
    };
    // The delivery runs in an isolated task and is never awaited by the event
    // loop, so a slow or hung CLI cannot stall event handling or the question.
    const task = (async () => {
      if (disposed) return;
      let child = null;
      try {
        child = spawn(argv, options);
        if (child && typeof child === "object") children.add(child);
        if (disposed) {
          if (child && typeof child.kill === "function") {
            try {
              child.kill();
            } catch (_) {}
          }
          return;
        }
        const exited = child && child.exited !== undefined ? child.exited : Promise.resolve(0);
        const code = await exited;
        if (typeof code === "number" && code !== 0 && !disposed) {
          report("phasegent: question notification delivery failed");
        }
      } catch (_) {
        // Fixed failure-category warning only: never the exception message,
        // process stderr, or any part of the event payload.
        if (!disposed) report("phasegent: question notification delivery failed");
      } finally {
        if (child) children.delete(child);
      }
    })();
    task.catch(() => {});
  };

  const handleEvent = async (event) => {
    if (disposed) return;
    if (!event || event.type !== "form.created") return;
    const form = event.data && event.data.form;
    if (!isQuestionForm(form)) return;
    if (!(await formBelongsToInstance(context, event, form, report))) return;
    if (seen.has(form.id)) return;
    rememberForm(seen, form.id);
    deliver(form);
  };

  const consume = (async () => {
    try {
      for await (const event of subscribe({ signal: controller.signal })) {
        if (disposed) break;
        try {
          await handleEvent(event);
        } catch (_) {
          if (!disposed) {
            report("phasegent: question notification handling failed");
          }
        }
      }
    } catch (_) {
      if (!disposed) {
        report("phasegent: question notification subscription failed");
      }
    }
  })();
  consume.catch(() => {});

  return () => {
    if (disposed) return;
    disposed = true;
    try {
      controller.abort();
    } catch (_) {}
    for (const child of children) {
      if (child && typeof child.kill === "function") {
        try {
          child.kill();
        } catch (_) {}
      }
    }
    children.clear();
  };
}
