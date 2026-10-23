// Focused tests for the OpenCode question-notification path (issue #1026 P1/P2).
//
// `createQuestionNotifier` is dependency-injected, so these run under `bun test`
// without OpenCode and without the phasegent CLI: the event subscription is a
// controllable async iterable and the child process is a fake spawn whose stdin
// pipe captures the structured JSON, so no real notification is ever delivered.

import { describe, expect, test } from "bun:test";
import { buildQuestionPayload, createQuestionNotifier } from "./src/notify.js";

const ROLE_MARKER = "phasegent-question-notify-test-role";
const SESSION_MARKER = "phasegent-question-notify-test-session";
const SENSITIVE_MARKER = "sensitive-marker-9f3a";

// A controllable AsyncIterable: `emit` pushes an event and `close` ends it.
function createEventStream() {
  const queue = [];
  let waiting = null;
  let closed = false;
  const iterator = {
    next() {
      if (queue.length > 0) return Promise.resolve({ value: queue.shift(), done: false });
      if (closed) return Promise.resolve({ value: undefined, done: true });
      return new Promise((resolve) => {
        waiting = resolve;
      });
    },
    return() {
      closed = true;
      if (waiting) {
        const resolve = waiting;
        waiting = null;
        resolve({ value: undefined, done: true });
      }
      return Promise.resolve({ value: undefined, done: true });
    },
  };
  return {
    stream: { [Symbol.asyncIterator]: () => iterator },
    emit(event) {
      if (closed) return;
      if (waiting) {
        const resolve = waiting;
        waiting = null;
        resolve({ value: event, done: false });
      } else {
        queue.push(event);
      }
    },
    close() {
      closed = true;
      if (waiting) {
        const resolve = waiting;
        waiting = null;
        resolve({ value: undefined, done: true });
      }
    },
  };
}

function makeChild({ exit = 0, exitPromise, writeThrows = false } = {}) {
  const child = {
    writes: [],
    ended: false,
    killed: false,
    stdin: {
      write(chunk) {
        if (writeThrows) throw new Error("stdin write failed");
        child.writes.push(String(chunk));
      },
      end() {
        child.ended = true;
      },
    },
    exited: exitPromise ?? Promise.resolve(exit),
    kill() {
      child.killed = true;
    },
  };
  return child;
}

function questionEvent({
  id = "frm_1",
  sessionID = "ses_1",
  metadata = { kind: "question" },
  fields,
  eventLocation,
  directory = "/repo",
} = {}) {
  const form = {
    id,
    sessionID,
    title: "Secret form title",
    metadata,
    fields: fields ?? [
      {
        key: "answer",
        type: "string",
        title: "Choice",
        description: "Which option?",
        options: [
          { label: "A", label_description: "ignored", description: "First option" },
          { label: "B" },
        ],
      },
    ],
  };
  const event = { type: "form.created", data: { form } };
  if (eventLocation !== undefined) {
    event.location = eventLocation;
  } else if (directory) {
    event.location = { directory };
  }
  return event;
}

function createHarness(options = {}) {
  const stream = createEventStream();
  const spawned = [];
  const warnMessages = [];
  const sessionGetInputs = [];
  const subscribeInfo = { calls: 0, options: null };

  const context = {
    location: options.location ?? { directory: "/repo" },
    session: {
      get: async (input) => {
        sessionGetInputs.push(input);
        if (options.sessionGetError) throw options.sessionGetError;
        if (typeof options.sessionGet === "function") return options.sessionGet(input);
        return {
          location: options.sessionLocation ?? { directory: "/repo" },
          title: options.sessionTitle ?? "",
        };
      },
    },
  };
  if (options.subscribe !== false) {
    context.event = {
      subscribe: (subscribeOptions) => {
        subscribeInfo.calls += 1;
        subscribeInfo.options = subscribeOptions;
        if (options.subscribeThrows) throw options.subscribeError ?? new Error("subscribe exploded");
        return stream.stream;
      },
    };
  }

  const fakeSpawn =
    options.spawn ??
    ((argv, spawnOptions) => (options.child ? options.child : makeChild()));
  const dependencies = {
    spawn: (argv, spawnOptions) => {
      const entry = { argv, options: spawnOptions, child: null };
      spawned.push(entry);
      const child = fakeSpawn(argv, spawnOptions);
      entry.child = child;
      return child;
    },
    warn: (message) => warnMessages.push(String(message)),
  };

  const dispose = createQuestionNotifier(context, dependencies);
  return { context, stream, spawned, warnMessages, sessionGetInputs, subscribeInfo, dispose };
}

async function settle() {
  for (let index = 0; index < 4; index += 1) {
    await new Promise((resolve) => setTimeout(resolve, 0));
  }
}

function payloadOf(entry) {
  return JSON.parse(entry.child.writes.join(""));
}

describe("question notifier structured delivery (issue #1026 P1/P2)", () => {
  test("sends one structured --stdin notification with a bounded JSON document", async () => {
    const harness = createHarness({
      location: { directory: "/repo", project: { directory: "/work/phasegent" } },
      sessionTitle: "Fix the parser",
    });
    harness.stream.emit(questionEvent());
    await settle();
    expect(harness.spawned).toHaveLength(1);
    const call = harness.spawned[0];
    expect(call.argv).toEqual(["phasegent", "notify", "send", "--stdin"]);
    expect(call.options.stdin).toBe("pipe");
    expect(call.options.timeout).toBe(10000);
    expect(call.options.cwd).toBe("/repo");
    expect(call.options.stdout).toBe("ignore");
    expect(call.options.stderr).toBe("ignore");
    // The payload never travels through argv.
    expect(call.argv.join(" ")).not.toContain("Which option?");
    expect(call.child.ended).toBe(true);
    const payload = payloadOf(call);
    expect(payload.event).toBe("blocked");
    expect(payload.question.session_id).toBe("ses_1");
    expect(payload.question.session_title).toBe("Fix the parser");
    expect(payload.question.project).toBe("phasegent");
    expect(payload.question.questions).toHaveLength(1);
    const question = payload.question.questions[0];
    expect(question.header).toBe("Choice");
    expect(question.text).toBe("Which option?");
    expect(question.multiple).toBe(false);
    expect(question.options).toEqual([
      { label: "A", description: "First option" },
      { label: "B", description: "" },
    ]);
    harness.dispose();
  });

  test("falls back to the session id when the title lookup fails", async () => {
    const harness = createHarness({ sessionGetError: new Error("no session info") });
    harness.stream.emit(questionEvent());
    await settle();
    expect(harness.spawned).toHaveLength(1);
    const payload = payloadOf(harness.spawned[0]);
    expect(payload.question.session_title).toBe("");
    expect(payload.question.session_id).toBe("ses_1");
    harness.dispose();
  });

  test("maps multiselect and uses the title as text fallback", async () => {
    const harness = createHarness();
    harness.stream.emit(
      questionEvent({
        fields: [
          { key: "pick", type: "multiselect", title: "Pick many", options: [{ label: "x" }] },
        ],
      }),
    );
    await settle();
    const question = payloadOf(harness.spawned[0]).question.questions[0];
    expect(question.multiple).toBe(true);
    expect(question.text).toBe("Pick many");
    expect(question.header).toBe("Pick many");
    harness.dispose();
  });

  test("derives project from the plugin directory basename when project is absent", async () => {
    const harness = createHarness({ location: { directory: "/repo/phasegent" } });
    harness.stream.emit(questionEvent({ directory: "/repo/phasegent" }));
    await settle();
    expect(payloadOf(harness.spawned[0]).question.project).toBe("phasegent");
    harness.dispose();
  });

  test("dedups a repeated form id into one call", async () => {
    const harness = createHarness();
    harness.stream.emit(questionEvent({ id: "frm_same" }));
    harness.stream.emit(questionEvent({ id: "frm_same" }));
    await settle();
    expect(harness.spawned).toHaveLength(1);
    harness.dispose();
  });

  test("notifies distinct form ids separately", async () => {
    const harness = createHarness();
    harness.stream.emit(questionEvent({ id: "frm_a" }));
    harness.stream.emit(questionEvent({ id: "frm_b" }));
    await settle();
    expect(harness.spawned).toHaveLength(2);
    harness.dispose();
  });

  test("accepts a generic agent name without a role lookup", async () => {
    const harness = createHarness();
    harness.stream.emit(
      questionEvent({ metadata: { kind: "question", agent: "some-random-agent" } }),
    );
    await settle();
    expect(harness.spawned).toHaveLength(1);
    harness.dispose();
  });

  test("skips a foreign location", async () => {
    const harness = createHarness();
    harness.stream.emit(questionEvent({ directory: "/elsewhere" }));
    await settle();
    expect(harness.spawned).toHaveLength(0);
    harness.dispose();
  });

  test("skips a workspace mismatch when both workspace ids are present", async () => {
    const harness = createHarness({ location: { directory: "/repo", workspaceID: "ws_1" } });
    harness.stream.emit(questionEvent({ eventLocation: { directory: "/repo", workspaceID: "ws_2" } }));
    await settle();
    expect(harness.spawned).toHaveLength(0);
    harness.dispose();
  });

  test("resolves an absent event location from the session and reuses it once", async () => {
    const harness = createHarness({ sessionTitle: "Reused title" });
    harness.stream.emit(questionEvent({ eventLocation: null }));
    harness.stream.emit(questionEvent({ id: "frm_2", eventLocation: null }));
    await settle();
    // One lookup per event, reused by delivery (no duplicate lookup).
    expect(harness.sessionGetInputs).toEqual([{ sessionID: "ses_1" }, { sessionID: "ses_1" }]);
    expect(harness.spawned).toHaveLength(2);
    expect(payloadOf(harness.spawned[0]).question.session_title).toBe("Reused title");
    harness.dispose();
  });

  test("skips an unresolvable location with a safe warning", async () => {
    const harness = createHarness({ sessionGetError: new Error("session unavailable") });
    harness.stream.emit(questionEvent({ eventLocation: null }));
    await settle();
    expect(harness.spawned).toHaveLength(0);
    expect(harness.warnMessages.join("\n")).toContain("form location unknown");
    harness.dispose();
  });

  test("a slow missing-location lookup does not delay a later local question", async () => {
    let resolveSlow;
    const slow = new Promise((resolve) => {
      resolveSlow = resolve;
    });
    const harness = createHarness({
      sessionGet: (input) =>
        input.sessionID === "ses_slow"
          ? slow
          : Promise.resolve({ location: { directory: "/repo" }, title: "Fast" }),
    });
    harness.stream.emit(questionEvent({ id: "frm_slow", sessionID: "ses_slow", eventLocation: null }));
    harness.stream.emit(questionEvent({ id: "frm_fast", sessionID: "ses_fast" }));
    await settle();
    // The later local question is delivered while the slow lookup is pending.
    expect(harness.spawned).toHaveLength(1);
    expect(payloadOf(harness.spawned[0]).question.session_id).toBe("ses_fast");
    resolveSlow({ location: { directory: "/repo" }, title: "Slow" });
    await settle();
    expect(harness.spawned).toHaveLength(2);
    harness.dispose();
  });

  test("a duplicate form does not start a second delivery while its lookup is pending", async () => {
    let resolveSlow;
    const slow = new Promise((resolve) => {
      resolveSlow = resolve;
    });
    const harness = createHarness({ sessionGet: () => slow });
    harness.stream.emit(questionEvent({ id: "frm_same", eventLocation: null }));
    harness.stream.emit(questionEvent({ id: "frm_same", eventLocation: null }));
    await settle();
    expect(harness.sessionGetInputs).toHaveLength(1);
    resolveSlow({ location: { directory: "/repo" }, title: "T" });
    await settle();
    expect(harness.spawned).toHaveLength(1);
    harness.dispose();
  });

  test("a missing location that resolves to another workspace is not sent", async () => {
    const harness = createHarness({ sessionLocation: { directory: "/elsewhere" } });
    harness.stream.emit(questionEvent({ id: "frm_foreign", eventLocation: null }));
    await settle();
    expect(harness.spawned).toHaveLength(0);
    harness.dispose();
  });

  test("disposal prevents a late spawn for a pending location lookup", async () => {
    let resolveSlow;
    const slow = new Promise((resolve) => {
      resolveSlow = resolve;
    });
    const harness = createHarness({ sessionGet: () => slow });
    harness.stream.emit(questionEvent({ id: "frm_slow", eventLocation: null }));
    await settle();
    expect(harness.spawned).toHaveLength(0);
    harness.dispose();
    resolveSlow({ location: { directory: "/repo" }, title: "Slow" });
    await settle();
    expect(harness.spawned).toHaveLength(0);
  });

  test("ignores non-question, malformed, replied and unrelated events", async () => {
    const harness = createHarness();
    harness.stream.emit(questionEvent({ metadata: { kind: "permission" } }));
    harness.stream.emit({ type: "form.created", data: {} });
    harness.stream.emit({ type: "form.created", data: { form: null } });
    harness.stream.emit({ type: "form.created", data: { form: { metadata: { kind: "question" } } } });
    harness.stream.emit({ type: "form.replied", data: { id: "frm_1" } });
    harness.stream.emit({ type: "permission.asked", data: { id: "perm_1" } });
    harness.stream.emit(null);
    await settle();
    expect(harness.spawned).toHaveLength(0);
    harness.dispose();
  });

  test("bounds the payload to the question and choice caps", async () => {
    const harness = createHarness();
    const manyOptions = Array.from({ length: 15 }, (_, index) => ({ label: `option ${index}` }));
    const fields = Array.from({ length: 10 }, (_, index) => ({
      key: `k${index}`,
      type: "string",
      title: `question ${index}`,
      options: manyOptions,
    }));
    harness.stream.emit(questionEvent({ fields }));
    await settle();
    const question = payloadOf(harness.spawned[0]).question;
    expect(question.questions).toHaveLength(8);
    expect(question.questions[0].options).toHaveLength(12);
    // The dropped entries are reported, never lost silently.
    expect(question.omitted_questions).toBe(2);
    expect(question.questions[0].omitted_options).toBe(3);
    expect(question.questions[1].omitted_options).toBe(3);
    harness.dispose();
  });

  test("reports every dropped entry of an oversized form", () => {
    const fields = Array.from({ length: 10 }, (_, index) => ({
      key: `k${index}`,
      type: "string",
      title: `question ${index}`,
      description: `text ${index}`,
      options: Array.from({ length: 15 }, (_, option) => ({ label: `option ${option}` })),
    }));
    const payload = buildQuestionPayload(
      { location: { directory: "/repo" } },
      { id: "frm_1", sessionID: "ses_1", fields },
      null,
    );
    expect(payload.question.questions).toHaveLength(8);
    expect(payload.question.omitted_questions).toBe(2);
    expect(payload.question.questions[0].options).toHaveLength(12);
    expect(payload.question.questions[0].omitted_options).toBe(3);
  });

  test("a header never starves the mandatory question text", () => {
    // The reviewer's exact boundary: a heavy first question leaves only 50
    // scalars, and the second question's 120-scalar header would consume them.
    const heavy = {
      type: "string",
      title: "h".repeat(120),
      description: "q".repeat(1500),
      options: Array.from({ length: 12 }, (_, index) => ({
        label: "l".repeat(160),
        description: "d".repeat(index === 11 ? 1 : 400),
      })),
    };
    const payload = buildQuestionPayload(
      { location: { directory: "/repo" } },
      {
        id: "frm_1",
        sessionID: "ses_1",
        fields: [heavy, { title: "H".repeat(120), description: "Actual second question?" }],
      },
      null,
    );
    const document = payload.question;
    const scalars = (value) => Array.from(value).length;
    const content =
      scalars(document.session_id) +
      scalars(document.session_title) +
      scalars(document.project) +
      document.questions.reduce(
        (total, question) =>
          total +
          scalars(question.header) +
          scalars(question.text) +
          question.options.reduce(
            (sum, option) => sum + scalars(option.label) + scalars(option.description),
            0,
          ),
        0,
      );
    expect(content).toBeLessThanOrEqual(8000);
    const last = document.questions.at(-1);
    // The real question text is retained (possibly visibly clipped), never
    // replaced by an invented fallback.
    expect(last.text.startsWith("Actual second question")).toBe(true);
    expect(document.questions.every((question) => question.text !== "Question")).toBe(true);
    expect(document.omitted_questions).toBe(0);
  });

  test("omits and counts a question that cannot retain real text", () => {
    const heavy = () => ({
      type: "string",
      title: "h".repeat(120),
      description: "q".repeat(1500),
      options: Array.from({ length: 12 }, (_, index) => ({
        label: "l".repeat(160),
        description: "d".repeat(index === 11 ? 1 : 400),
      })),
    });
    const payload = buildQuestionPayload(
      { location: { directory: "/repo" } },
      { id: "frm_1", sessionID: "ses_1", fields: [heavy(), heavy(), heavy()] },
      null,
    );
    const document = payload.question;
    expect(document.questions.length).toBeLessThan(3);
    expect(document.omitted_questions).toBeGreaterThan(0);
    expect(
      document.questions.every(
        (question) => question.text.length > 0 && question.text !== "…" && question.text !== "Question",
      ),
    ).toBe(true);
  });

  test("never invents a question for a field without real text", async () => {
    const harness = createHarness();
    harness.stream.emit(
      questionEvent({ fields: [{ key: "empty", type: "string", title: "", description: "" }] }),
    );
    await settle();
    // Nothing real to say: no notification, and no synthetic question text.
    expect(harness.spawned).toHaveLength(0);
    expect(harness.warnMessages.join("\n")).toContain("question notification skipped");
    harness.dispose();
  });

  test("counts entries dropped by the shared budget as omissions", () => {
    const fields = Array.from({ length: 4 }, (_, index) => ({
      key: `k${index}`,
      type: "string",
      title: `question ${index}`,
      description: "y".repeat(1500),
      options: Array.from({ length: 6 }, (_, option) => ({
        label: `option ${option}`,
        description: "z".repeat(400),
      })),
    }));
    const payload = buildQuestionPayload(
      { location: { directory: "/repo" } },
      { id: "frm_1", sessionID: "ses_1", fields },
      null,
    );
    const omitted = payload.question.questions.reduce(
      (total, question) => total + question.omitted_options,
      0,
    );
    expect(omitted).toBeGreaterThan(0);
    expect(payload.question.omitted_questions).toBeGreaterThan(0);
  });

  test("clips an oversized field with an ellipsis", () => {
    const payload = buildQuestionPayload(
      { location: { directory: "/repo" } },
      { id: "frm", sessionID: "ses_1", fields: [{ title: "h", text: "x".repeat(5000), description: "y".repeat(5000) }] },
      null,
    );
    const question = payload.question.questions[0];
    expect(question.text.length).toBeLessThanOrEqual(1500);
    expect(question.text.endsWith("…")).toBe(true);
  });
});

describe("question notifier failure handling (issue #1026 P1/P2)", () => {
  test("keeps the inherited environment unchanged, including role and session markers", async () => {
    const savedRole = process.env.PHASEGENT_ROLE;
    const savedSession = process.env.PHASEGENT_SESSION_ID;
    process.env.PHASEGENT_ROLE = ROLE_MARKER;
    process.env.PHASEGENT_SESSION_ID = SESSION_MARKER;
    const harness = createHarness();
    try {
      harness.stream.emit(questionEvent());
      await settle();
      expect(harness.spawned).toHaveLength(1);
      const options = harness.spawned[0].options;
      expect(options.env).toBe(process.env);
      expect(options.env.PHASEGENT_ROLE).toBe(ROLE_MARKER);
      expect(options.env.PHASEGENT_SESSION_ID).toBe(SESSION_MARKER);
    } finally {
      harness.dispose();
      if (savedRole === undefined) delete process.env.PHASEGENT_ROLE;
      else process.env.PHASEGENT_ROLE = savedRole;
      if (savedSession === undefined) delete process.env.PHASEGENT_SESSION_ID;
      else process.env.PHASEGENT_SESSION_ID = savedSession;
    }
  });

  test("survives a throwing spawn error and still notifies a later question", async () => {
    let first = true;
    const harness = createHarness({
      spawn: () => {
        if (first) {
          first = false;
          throw new Error(`spawn unavailable ${SENSITIVE_MARKER}\ninjected-line`);
        }
        return makeChild();
      },
    });
    harness.stream.emit(questionEvent({ id: "frm_throw" }));
    await settle();
    expect(harness.warnMessages.join("\n")).toContain("question notification delivery failed");
    harness.stream.emit(questionEvent({ id: "frm_after" }));
    await settle();
    expect(harness.spawned).toHaveLength(2);
    harness.dispose();
  });

  test("sanitizes a thrown spawn error to a fixed failure category", async () => {
    const harness = createHarness({
      spawn: () => {
        throw new Error(`spawn failed ${SENSITIVE_MARKER}\ninjected-line`);
      },
    });
    harness.stream.emit(questionEvent({ id: "frm_throw_secret" }));
    await settle();
    const joined = harness.warnMessages.join("\n");
    expect(joined).toContain("question notification delivery failed");
    expect(joined).not.toContain(SENSITIVE_MARKER);
    expect(joined).not.toContain("injected-line");
    harness.dispose();
  });

  test("sanitizes a rejected child exit to a fixed failure category", async () => {
    const harness = createHarness({
      spawn: () =>
        makeChild({ exitPromise: Promise.reject(new Error(`exit ${SENSITIVE_MARKER}\ninjected`)) }),
    });
    harness.stream.emit(questionEvent({ id: "frm_reject_secret" }));
    await settle();
    const joined = harness.warnMessages.join("\n");
    expect(joined).toContain("question notification delivery failed");
    expect(joined).not.toContain(SENSITIVE_MARKER);
    expect(joined).not.toContain("injected");
    harness.dispose();
  });

  test("survives a nonzero exit and still notifies a later question", async () => {
    const harness = createHarness({ spawn: () => makeChild({ exit: 2 }) });
    harness.stream.emit(questionEvent({ id: "frm_nonzero" }));
    await settle();
    expect(harness.warnMessages.join("\n")).toContain("question notification delivery failed");
    harness.stream.emit(questionEvent({ id: "frm_next" }));
    await settle();
    expect(harness.spawned).toHaveLength(2);
    harness.dispose();
  });

  test("reports a stdin write failure without failing the question", async () => {
    const harness = createHarness({ spawn: () => makeChild({ writeThrows: true }) });
    harness.stream.emit(questionEvent({ id: "frm_write" }));
    await settle();
    expect(harness.warnMessages.join("\n")).toContain("question notification delivery failed");
    harness.dispose();
  });

  test("closes the pipe only after an asynchronous write has flushed", async () => {
    const order = [];
    const child = {
      writes: [],
      stdin: {
        write(chunk) {
          order.push("write");
          child.writes.push(String(chunk));
          return new Promise((resolve) =>
            setTimeout(() => {
              order.push("flushed");
              resolve(child.writes.length);
            }, 0),
          );
        },
        end() {
          order.push("end");
        },
      },
      exited: Promise.resolve(0),
      kill() {},
    };
    const harness = createHarness({ spawn: () => child });
    harness.stream.emit(questionEvent({ id: "frm_async_write" }));
    await settle();
    expect(order).toEqual(["write", "flushed", "end"]);
    expect(JSON.parse(child.writes.join("")).question.session_id).toBe("ses_1");
    harness.dispose();
  });

  test("terminates the owned child at once when the stdin write fails", async () => {
    const children = [];
    const harness = createHarness({
      spawn: () => {
        const child = makeChild({ writeThrows: true, exitPromise: new Promise(() => {}) });
        child.kills = 0;
        child.kill = () => {
          child.kills += 1;
          child.killed = true;
        };
        children.push(child);
        return child;
      },
    });
    harness.stream.emit(questionEvent({ id: "frm_write_fail" }));
    await settle();
    expect(harness.warnMessages.join("\n")).toContain("question notification delivery failed");
    // The pipe is closed and the child is terminated without waiting for an
    // exit that may never arrive.
    expect(children[0].ended).toBe(true);
    expect(children[0].kills).toBe(1);
    // Disposal does not clean the terminated child up a second time.
    harness.dispose();
    expect(children[0].kills).toBe(1);
  });

  test("a failed delivery does not block the next question", async () => {
    const harness = createHarness({
      spawn: () => makeChild({ writeThrows: true, exitPromise: new Promise(() => {}) }),
    });
    harness.stream.emit(questionEvent({ id: "frm_write_fail" }));
    harness.stream.emit(questionEvent({ id: "frm_write_next" }));
    await settle();
    expect(harness.spawned).toHaveLength(2);
    harness.dispose();
  });

  test("never blocks event handling on process completion", async () => {
    const harness = createHarness({
      spawn: () => makeChild({ exitPromise: new Promise(() => {}) }),
    });
    harness.stream.emit(questionEvent({ id: "frm_hung_a" }));
    harness.stream.emit(questionEvent({ id: "frm_hung_b" }));
    await settle();
    expect(harness.spawned).toHaveLength(2);
    harness.dispose();
  });

  test("reports a subscription error safely", async () => {
    const harness = createHarness({
      subscribeThrows: true,
      subscribeError: new Error(`subscribe ${SENSITIVE_MARKER}\ninjected`),
    });
    await settle();
    const joined = harness.warnMessages.join("\n");
    expect(joined).toContain("question notification subscription failed");
    expect(joined).not.toContain(SENSITIVE_MARKER);
    expect(joined).not.toContain("injected");
    expect(() => harness.dispose()).not.toThrow();
  });

  test("sanitizes a session lookup error to a fixed failure category", async () => {
    const harness = createHarness({
      sessionGetError: new Error(`lookup ${SENSITIVE_MARKER}\ninjected-line`),
    });
    harness.stream.emit(questionEvent({ eventLocation: null }));
    await settle();
    const joined = harness.warnMessages.join("\n");
    expect(joined).toContain("form location unknown");
    expect(joined).not.toContain(SENSITIVE_MARKER);
    expect(joined).not.toContain("injected-line");
    harness.dispose();
  });

  test("sanitizes a handling error to a fixed failure category", async () => {
    const harness = createHarness();
    const evil = { type: "form.created" };
    Object.defineProperty(evil, "data", {
      get() {
        throw new Error(`handling ${SENSITIVE_MARKER}\ninjected-line`);
      },
    });
    harness.stream.emit(evil);
    await settle();
    const joined = harness.warnMessages.join("\n");
    expect(joined).toContain("question notification handling failed");
    expect(joined).not.toContain(SENSITIVE_MARKER);
    expect(joined).not.toContain("injected-line");
    harness.dispose();
  });

  test("warns once and returns a harmless disposer without an event subscription", async () => {
    const harness = createHarness({ subscribe: false });
    expect(harness.warnMessages.join("\n")).toContain("no event subscription");
    expect(typeof harness.dispose).toBe("function");
    expect(() => harness.dispose()).not.toThrow();
    await settle();
    expect(harness.spawned).toHaveLength(0);
  });

  test("dispose aborts, terminates owned children and prevents future spawns", async () => {
    const harness = createHarness({
      spawn: () => makeChild({ exitPromise: new Promise(() => {}) }),
    });
    harness.stream.emit(questionEvent({ id: "frm_owned" }));
    await settle();
    expect(harness.spawned).toHaveLength(1);
    const child = harness.spawned[0].child;
    harness.dispose();
    expect(child.killed).toBe(true);
    expect(harness.subscribeInfo.options.signal.aborted).toBe(true);
    harness.stream.emit(questionEvent({ id: "frm_after_dispose" }));
    await settle();
    expect(harness.spawned).toHaveLength(1);
  });
});
