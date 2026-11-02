// Best-effort question-awaiting-user notification (issue #1026 P1/P2).
//
// The adapter subscribes to the host event stream and tracks each *new* local
// pending question form. A form still unanswered after five minutes invokes the
// existing `phasegent notify send --stdin` CLI once with a structured, bounded
// JSON document on stdin; reply, cancellation, session deletion, disposal, and
// stream loss cancel the pending reminder. It never answers a question, never
// blocks or fails the question tool, and never inspects credentials or local
// databases: delivery is one bounded, argv-only child process that reuses the
// operator's configured channel and inherits the environment unchanged
// (including any role/session markers the ambient caller already carries).
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
export const QUESTION_NOTIFY_DELAY_MS = 300000;

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
// console warning channel. `dependencies.schedule`/`cancel`/`now` are the
// injectable timer seam, defaulting to `setTimeout`/`clearTimeout`/`Date.now`.
export function createQuestionNotifier(context, dependencies = {}) {
  const spawn = typeof dependencies.spawn === "function" ? dependencies.spawn : defaultSpawn;
  const report = typeof dependencies.warn === "function" ? dependencies.warn : warn;
  const schedule =
    typeof dependencies.schedule === "function"
      ? dependencies.schedule
      : (callback, delay) => setTimeout(callback, delay);
  const cancel =
    typeof dependencies.cancel === "function"
      ? dependencies.cancel
      : (handle) => clearTimeout(handle);
  const now = typeof dependencies.now === "function" ? dependencies.now : Date.now;
  const subscribe = context && context.event && context.event.subscribe;
  if (typeof subscribe !== "function") {
    report("phasegent: host exposes no event subscription; question notifications are disabled");
    return () => {};
  }

  const controller = new AbortController();
  const pending = new Map();
  const settled = new Set();
  const deletedSessions = new Set();
  const children = new Set();
  let disposed = false;
  let streamEnded = false;

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

  const clearTimer = (entry) => {
    if (entry && entry.timer !== null) {
      cancel(entry.timer);
      entry.timer = null;
    }
  };

  // A resolved identifier stays a compact lifetime tombstone even when its
  // create event was not tracked yet, so a late or replayed create can never
  // rearm an answered or delivered form. The tombstone holds no form payload and
  // is released with the notifier instance.
  const settleForm = (id) => {
    if (typeof id !== "string" || id.length === 0) return;
    settled.add(id);
    const entry = pending.get(id);
    if (!entry) return;
    entry.settled = true;
    clearTimer(entry);
    pending.delete(id);
  };

  // A deleted session is remembered independently of its current pending forms.
  // It rejects later creates for that session and releases matching timers and
  // payload entries at once.
  const settleSession = (sessionID) => {
    if (typeof sessionID !== "string" || sessionID.length === 0) return;
    deletedSessions.add(sessionID);
    for (const [id, entry] of pending) {
      if (entry.sessionID === sessionID) settleForm(id);
    }
  };

  const clearPending = () => {
    for (const entry of pending.values()) {
      entry.settled = true;
      clearTimer(entry);
    }
    pending.clear();
  };

  const isActive = (entry) =>
    !disposed &&
    !streamEnded &&
    !entry.settled &&
    pending.get(entry.id) === entry;

  const deliver = (entry) => {
    // Each delivery runs in an isolated task and is never awaited by the event
    // loop, so a slow or hung CLI (or session-title lookup) cannot stall event
    // handling or another question's deadline.
    const task = (async () => {
      if (!isActive(entry) || entry.delivering) return;
      entry.delivering = true;
      let child = null;
      try {
        // Reuse the session already fetched during location validation. A form
        // whose directory arrived with the event fetches only its title here.
        const enriched =
          entry.session || (await readFormSession(context, entry.form.sessionID));
        if (!isActive(entry)) return;
        const document = buildQuestionPayload(context, entry.form, enriched);
        if (document.question.questions.length === 0) {
          // Nothing but omissions: a question notification without a single
          // real question would have to invent one, so it is not sent.
          report("phasegent: question notification skipped: no question text");
          settleForm(entry.id);
          return;
        }
        // Resolution events are handled synchronously, so this is the last
        // eligibility check before the CLI is spawned.
        if (!isActive(entry)) return;
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
        settleForm(entry.id);
        if (disposed || streamEnded) {
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
        if (pending.get(entry.id) === entry) settleForm(entry.id);
      }
    })();
    task.catch(() => {});
  };

  const onTimer = (entry) => {
    entry.timer = null;
    if (!isActive(entry)) return;
    const remaining = entry.deadline - now();
    if (remaining > 0) {
      entry.timer = schedule(() => onTimer(entry), remaining);
      return;
    }
    // A missing-location lookup may still be running. Waiting for this entry's
    // own lookup never delays another question, and the original deadline is
    // kept because no new deadline is recorded.
    const task = (async () => {
      if (entry.ready) await entry.ready;
      if (!isActive(entry) || entry.local !== true) return;
      deliver(entry);
    })();
    task.catch(() => {
      if (!disposed) report("phasegent: question notification handling failed");
    });
  };

  const track = (event, form) => {
    if (pending.has(form.id) || settled.has(form.id)) return null;
    const eventLocation =
      event.location && typeof event.location === "object" ? event.location : null;
    const hasEventDirectory = Boolean(
      eventLocation && typeof eventLocation.directory === "string",
    );
    const entry = {
      id: form.id,
      sessionID: typeof form.sessionID === "string" ? form.sessionID : "",
      form,
      event,
      session: null,
      local: hasEventDirectory ? true : null,
      settled: false,
      delivering: false,
      deadline: now() + QUESTION_NOTIFY_DELAY_MS,
      timer: null,
      ready: null,
    };
    pending.set(form.id, entry);
    entry.timer = schedule(() => onTimer(entry), QUESTION_NOTIFY_DELAY_MS);
    if (!hasEventDirectory) resolveLocation(entry);
    return entry;
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

  // Resolve a missing event location in the entry's own task. The pending entry
  // is already armed, so a duplicate created event neither resets the deadline
  // nor starts a second lookup.
  const resolveLocation = (entry) => {
    entry.ready = (async () => {
      const session = await readFormSession(context, entry.form.sessionID);
      if (!isActive(entry)) return null;
      if (!formBelongsToInstance(entry.event, session)) {
        settleForm(entry.id);
        return null;
      }
      entry.session = session;
      entry.local = true;
      return session;
    })().catch(() => {
      if (isActive(entry)) {
        settleForm(entry.id);
        report("phasegent: question notification handling failed");
      }
      return null;
    });
  };

  const handleEvent = (event) => {
    if (disposed || streamEnded) return;
    if (!event || typeof event !== "object") return;
    if (event.type === "form.replied" || event.type === "form.cancelled") {
      const data = event.data && typeof event.data === "object" ? event.data : null;
      settleForm(data && data.id);
      return;
    }
    if (event.type === "session.deleted") {
      const data = event.data && typeof event.data === "object" ? event.data : null;
      settleSession(data && data.sessionID);
      return;
    }
    if (event.type !== "form.created") return;
    const form = event.data && event.data.form;
    if (!isQuestionForm(form)) return;
    if (pending.has(form.id) || settled.has(form.id)) return;
    if (deletedSessions.has(form.sessionID)) return;
    const eventLocation =
      event.location && typeof event.location === "object" ? event.location : null;
    const hasEventDirectory = Boolean(
      eventLocation && typeof eventLocation.directory === "string",
    );
    if (hasEventDirectory && !formBelongsToInstance(event, null)) return;
    track(event, form);
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
    } finally {
      streamEnded = true;
      clearPending();
    }
  })();
  consume.catch(() => {});

  return () => {
    if (disposed) return;
    disposed = true;
    clearPending();
    settled.clear();
    deletedSessions.clear();
    try {
      controller.abort();
    } catch (_) {}
    for (const child of children) killChild(child);
    children.clear();
  };
}
