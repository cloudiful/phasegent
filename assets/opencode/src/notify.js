// Best-effort question-awaiting-user notification (issue #1026 P1/P2).
//
// The adapter subscribes to the host event stream and, for each *new* local
// pending question form, invokes the existing `phasegent notify send --stdin`
// CLI once with a structured, bounded JSON document on stdin. It never answers
// a question, never blocks or fails the question tool, and never inspects
// credentials or local databases: delivery is one bounded, argv-only child
// process that reuses the operator's configured channel and inherits the
// environment unchanged (including any role/session markers the ambient caller
// already carries).
//
// The official v2 subscription is an AsyncIterable (`context.event.subscribe`),
// the payload lives in `event.data.form`, and a question is a `form.created`
// whose `metadata.kind` is `"question"`. Only events whose location matches this
// plugin instance are sent, so a shared host never notifies for another
// workspace's question. The JSON never travels through argv, a file, or a
// shell.

import { locationDirectory, warn } from "./runtime.js";

export const QUESTION_NOTIFY_TITLE = "OpenCode question pending";
export const QUESTION_NOTIFY_TIMEOUT_MS = 10000;
export const QUESTION_DEDUP_LIMIT = 256;

// Keep the plugin's proactive bounds equivalent to the CLI normalization so a
// normal oversized form produces a bounded readable notification instead of a
// CLI rejection.
export const QUESTION_INPUT_LIMIT_BYTES = 65536;
export const MAX_QUESTIONS = 8;
export const MAX_CHOICES = 12;
export const TOTAL_TEXT_BUDGET = 8000;

const CAPS = {
  sessionId: 128,
  sessionTitle: 200,
  project: 240,
  header: 120,
  text: 1500,
  label: 160,
  description: 400,
};

const ARGV = ["phasegent", "notify", "send", "--stdin"];

function defaultSpawn(argv, options) {
  return Bun.spawn(argv, options);
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

function cleanText(raw) {
  if (typeof raw !== "string" || raw.length === 0) return "";
  return raw
    .replace(/\r\n/g, "\n")
    .replace(/\r/g, "\n")
    .replace(/[\u0000-\u0009\u000b-\u001f\u007f]/g, "")
    .trim();
}

// Charge `raw` against the shared budget and clip to `cap` scalars, appending a
// single ellipsis inside the cap when it does not fit. Never returns partial
// structure, only text.
function takeField(budget, raw, cap) {
  const cleaned = cleanText(raw);
  if (!cleaned) return "";
  const available = Math.min(cap, budget.remaining);
  if (available <= 0) return "";
  const chars = Array.from(cleaned);
  if (chars.length <= available) {
    budget.remaining -= chars.length;
    return cleaned;
  }
  const keep = Math.max(available - 1, 0);
  const clipped = `${chars.slice(0, keep).join("")}…`;
  budget.remaining -= Array.from(clipped).length;
  return clipped;
}

// Take the mandatory question text. Only real source content counts: a clip
// that would leave nothing but the ellipsis carries no question, so the text
// is not taken at all and the caller omits the question instead.
function takeQuestionText(budget, raw, cap) {
  const cleaned = cleanText(raw);
  if (!cleaned) return "";
  const available = Math.min(cap, budget.remaining);
  if (available < 2) return "";
  const chars = Array.from(cleaned);
  if (chars.length <= available) {
    budget.remaining -= chars.length;
    return cleaned;
  }
  const clipped = `${chars.slice(0, available - 1).join("")}…`;
  budget.remaining -= Array.from(clipped).length;
  return clipped;
}

function basename(value) {
  if (typeof value !== "string" || value.length === 0) return "";
  const trimmed = value.replace(/[\\/]+$/, "");
  const index = Math.max(trimmed.lastIndexOf("/"), trimmed.lastIndexOf("\\"));
  return index >= 0 ? trimmed.slice(index + 1) : trimmed;
}

// Project name is the basename of the host project directory, falling back to
// the plugin directory basename. The full absolute path is never disclosed.
function projectName(context) {
  const location = context && context.location;
  const projectDirectory =
    location && location.project && typeof location.project.directory === "string"
      ? location.project.directory
      : "";
  return basename(projectDirectory) || basename(locationDirectory(context));
}

async function readFormSession(context, sessionID) {
  const get = context && context.session && context.session.get;
  if (typeof get !== "function") return null;
  if (typeof sessionID !== "string" || sessionID.length === 0) return null;
  try {
    const result = await get({ sessionID });
    if (!result || typeof result !== "object") return null;
    const location = result.location && typeof result.location === "object" ? result.location : null;
    const directory = location && typeof location.directory === "string" ? location.directory : null;
    const workspaceID =
      location && typeof location.workspaceID === "string" ? location.workspaceID : null;
    const title = typeof result.title === "string" ? result.title : "";
    if (!directory && !title) return null;
    return { directory, workspaceID, title };
  } catch (_) {
    return null;
  }
}

// Build the bounded structured document the CLI parses. Field mapping: the form
// field title is the question header, the field description is the question
// text (falling back to the title), and a multiselect field maps to `multiple`.
// Empty optional strings stay empty.
//
// Every entry the plugin drops — beyond the question/choice caps, beyond the
// shared budget, missing real question text, or empty — is counted and reported
// as an omission so the recipient can see that content is missing instead of
// silently losing it. No question is ever rendered with invented text.
export function buildQuestionPayload(context, form, session) {
  const budget = { remaining: TOTAL_TEXT_BUDGET };
  const sessionId = takeField(budget, form && form.sessionID, CAPS.sessionId) || "unknown";
  const sessionTitle = takeField(budget, session && session.title, CAPS.sessionTitle);
  const project = takeField(budget, projectName(context), CAPS.project);
  const fields = Array.isArray(form && form.fields) ? form.fields : [];
  const questions = [];
  let omittedQuestions = 0;
  for (const field of fields) {
    if (questions.length >= MAX_QUESTIONS) {
      omittedQuestions += 1;
      continue;
    }
    // The question text is mandatory, so it is allocated before the optional
    // header and choices: neither of them can starve it.
    let text = takeQuestionText(budget, field && field.description, CAPS.text);
    if (!text) text = takeQuestionText(budget, field && field.title, CAPS.text);
    if (!text) {
      // No real question text: the question is omitted and counted rather
      // than rendered with a stand-in.
      omittedQuestions += 1;
      continue;
    }
    const header = takeField(budget, field && field.title, CAPS.header);
    const rawOptions = Array.isArray(field && field.options) ? field.options : [];
    const options = [];
    let omittedOptions = 0;
    for (const option of rawOptions) {
      if (options.length >= MAX_CHOICES || budget.remaining <= 0) {
        omittedOptions += 1;
        continue;
      }
      const label = takeField(budget, option && option.label, CAPS.label);
      const description = takeField(budget, option && option.description, CAPS.description);
      if (!label && !description) {
        omittedOptions += 1;
        continue;
      }
      options.push({ label, description });
    }
    questions.push({
      header,
      text,
      multiple: Boolean(field && field.type === "multiselect"),
      options,
      omitted_options: omittedOptions,
    });
  }
  // `questions` may be empty: every field was dropped for lack of real text or
  // budget, which `omitted_questions` reports. The caller skips delivery for an
  // empty list instead of inventing a question to satisfy the CLI.
  return {
    event: "blocked",
    question: {
      session_id: sessionId,
      session_title: sessionTitle,
      project,
      questions,
      omitted_questions: omittedQuestions,
    },
  };
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
  const inFlight = new Set();
  let disposed = false;

  const killChild = (child) => {
    if (child && typeof child.kill === "function") {
      try {
        child.kill();
      } catch (_) {}
    }
  };

  // Close the pipe and terminate the owned child. The exit is not awaited here:
  // a failed delivery may never produce one, and the child is released from the
  // owned set so disposal cannot clean it up twice.
  const terminateChild = (child) => {
    if (!child) return;
    children.delete(child);
    const exited = child.exited;
    if (exited && typeof exited.catch === "function") exited.catch(() => {});
    const sink = child.stdin;
    if (sink && typeof sink.end === "function") {
      try {
        sink.end();
      } catch (_) {}
    }
    killChild(child);
  };

  // Write the payload and close the pipe. A sink may report the write
  // asynchronously, so the write is awaited before the pipe is closed; a
  // pending write would otherwise be truncated by the close.
  const writeStdin = async (child, text) => {
    const sink = child && child.stdin;
    if (!sink || typeof sink.write !== "function") return;
    const written = sink.write(text);
    if (written && typeof written.then === "function") await written;
    if (typeof sink.end === "function") sink.end();
  };

  const deliver = (form, session) => {
    // Each delivery runs in an isolated task and is never awaited by the event
    // loop, so a slow or hung CLI (or session-title lookup) cannot stall event
    // handling or the question.
    const task = (async () => {
      if (disposed) return;
      let child = null;
      try {
        // Reuse the session already fetched during location validation; only
        // fetch for the title here when event.location carried the directory.
        const enriched = session || (await readFormSession(context, form.sessionID));
        if (disposed) return;
        const document = buildQuestionPayload(context, form, enriched);
        if (document.question.questions.length === 0) {
          // Nothing but omissions: a question notification without a single
          // real question would have to invent one, so it is not sent.
          report("phasegent: question notification skipped: no question text");
          return;
        }
        const payload = JSON.stringify(document);
        const cwd = locationDirectory(context);
        child = spawn(ARGV, {
          cwd: cwd || undefined,
          env: process.env,
          stdin: "pipe",
          stdout: "ignore",
          stderr: "ignore",
          timeout: QUESTION_NOTIFY_TIMEOUT_MS,
        });
        if (child && typeof child === "object") children.add(child);
        if (disposed) {
          killChild(child);
          return;
        }
        try {
          await writeStdin(child, payload);
        } catch (_) {
          // A failed write is a delivery failure, not a question failure: the
          // pipe is closed and the owned child is terminated at once instead of
          // waiting for an exit that may never arrive.
          if (!disposed) report("phasegent: question notification delivery failed");
          terminateChild(child);
          return;
        }
        const exited = child && child.exited !== undefined ? child.exited : Promise.resolve(0);
        const code = await exited;
        if (typeof code === "number" && code !== 0 && !disposed) {
          report("phasegent: question notification delivery failed");
        }
      } catch (_) {
        if (!disposed) report("phasegent: question notification delivery failed");
        terminateChild(child);
      } finally {
        if (child) children.delete(child);
      }
    })();
    task.catch(() => {});
  };

  // The form belongs to this plugin instance only when its directory equals the
  // plugin's own location; a missing/mismatched/unresolvable location skips (with
  // a safe warning when unknown), so a shared host never notifies cross-location.
  const formBelongsToInstance = (event, session) => {
    const own = locationDirectory(context);
    if (!own) {
      report("phasegent: question notification skipped: plugin location unavailable");
      return false;
    }
    const eventLocation =
      event.location && typeof event.location === "object" ? event.location : null;
    const directory =
      (eventLocation && typeof eventLocation.directory === "string"
        ? eventLocation.directory
        : null) ||
      (session && session.directory) ||
      null;
    if (!directory) {
      report("phasegent: question notification skipped: form location unknown");
      return false;
    }
    if (directory !== own) return false;
    const workspaceID =
      (eventLocation && typeof eventLocation.workspaceID === "string"
        ? eventLocation.workspaceID
        : null) || (session && session.workspaceID) || null;
    const ownWorkspace =
      context.location && typeof context.location.workspaceID === "string"
        ? context.location.workspaceID
        : null;
    if (workspaceID && ownWorkspace && workspaceID !== ownWorkspace) return false;
    return true;
  };

  // A form whose event carries no location is resolved in its own task, so one
  // slow session lookup never delays another question. The form id is reserved
  // while the lookup runs, so a duplicate event cannot start a second delivery.
  const resolveMissingLocation = (event, form) => {
    if (inFlight.has(form.id)) return;
    inFlight.add(form.id);
    const task = (async () => {
      const session = await readFormSession(context, form.sessionID);
      if (disposed) return;
      if (!formBelongsToInstance(event, session)) return;
      if (seen.has(form.id)) return;
      rememberForm(seen, form.id);
      deliver(form, session);
    })();
    task.then(
      () => {
        inFlight.delete(form.id);
      },
      () => {
        inFlight.delete(form.id);
        if (!disposed) report("phasegent: question notification handling failed");
      },
    );
  };

  const handleEvent = (event) => {
    if (disposed) return;
    if (!event || event.type !== "form.created") return;
    const form = event.data && event.data.form;
    if (!isQuestionForm(form)) return;
    const eventLocation =
      event.location && typeof event.location === "object" ? event.location : null;
    const hasEventDirectory = Boolean(
      eventLocation && typeof eventLocation.directory === "string",
    );
    if (!hasEventDirectory) {
      // The location is unknown, so it is resolved independently of the stream.
      if (seen.has(form.id)) return;
      resolveMissingLocation(event, form);
      return;
    }
    if (!formBelongsToInstance(event, null)) return;
    if (seen.has(form.id)) return;
    rememberForm(seen, form.id);
    deliver(form, null);
  };

  const consume = (async () => {
    try {
      for await (const event of subscribe({ signal: controller.signal })) {
        if (disposed) break;
        try {
          handleEvent(event);
        } catch (_) {
          if (!disposed) report("phasegent: question notification handling failed");
        }
      }
    } catch (_) {
      if (!disposed) report("phasegent: question notification subscription failed");
    }
  })();
  consume.catch(() => {});

  return () => {
    if (disposed) return;
    disposed = true;
    try {
      controller.abort();
    } catch (_) {}
    for (const child of children) killChild(child);
    children.clear();
    inFlight.clear();
  };
}
