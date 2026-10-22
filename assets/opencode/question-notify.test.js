// Focused tests for the OpenCode question-notification path (issue #1026 P1).
//
// `createQuestionNotifier` is dependency-injected, so these run under `bun test`
// without OpenCode and without the phasegent CLI: the event subscription is a
// controllable async iterable and the child process is a fake spawn, so no
// real notification is ever delivered.

import { describe, expect, test } from "bun:test";
import { createQuestionNotifier } from "./src/notify.js";

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

function questionEvent({
  id = "frm_1",
  sessionID = "ses_1",
  directory = "/repo",
  workspaceID,
  metadata = { kind: "question" },
  title = "Pick one secret title",
  fields,
  eventLocation,
} = {}) {
  const form = {
    id,
    sessionID,
    title,
    metadata,
    fields: fields ?? [
      {
        key: "answer",
        type: "string",
        title: "SECRET FIELD TITLE",
        options: [{ value: "opt", label: "SECRET OPTION LABEL" }],
      },
    ],
  };
  const event = { type: "form.created", data: { form } };
  if (eventLocation !== undefined) {
    event.location = eventLocation;
  } else if (directory) {
    event.location = workspaceID ? { directory, workspaceID } : { directory };
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
        if (options.sessionLocation === null) return { location: {} };
        return { location: options.sessionLocation ?? { directory: "/repo" } };
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

  const fakeSpawn = options.spawn ?? (() => ({ exited: Promise.resolve(0), kill() {} }));
  const dependencies = {
    spawn: (argv, spawnOptions) => {
      spawned.push({ argv, options: spawnOptions });
      return fakeSpawn(argv, spawnOptions);
    },
    warn: (message) => warnMessages.push(String(message)),
  };

  const dispose = createQuestionNotifier(context, dependencies);
  return { context, stream, spawned, warnMessages, sessionGetInputs, subscribeInfo, dispose };
}

async function settle() {
  for (let index = 0; index < 3; index += 1) {
    await new Promise((resolve) => setTimeout(resolve, 0));
  }
}

function bodyOf(call) {
  const index = call.argv.indexOf("--body");
  return index >= 0 ? call.argv[index + 1] : "";
}

describe("question notifier delivery (issue #1026 P1)", () => {
  test("sends exactly one argv-only blocked notification for a local question", async () => {
    const harness = createHarness();
    harness.stream.emit(questionEvent());
    await settle();
    expect(harness.spawned).toHaveLength(1);
    const call = harness.spawned[0];
    expect(Array.isArray(call.argv)).toBe(true);
    expect(call.argv).toEqual([
      "phasegent",
      "notify",
      "send",
      "--event",
      "blocked",
      "--title",
      "OpenCode question pending",
      "--body",
      bodyOf(call),
    ]);
    expect(call.argv).not.toContain("sh");
    expect(call.argv).not.toContain("-c");
    const body = bodyOf(call);
    expect(body).toContain("ses_1");
    expect(body).toContain("1 field");
    expect(body).toContain("Answer it in OpenCode");
    expect(body).not.toContain("Pick one secret title");
    expect(body).not.toContain("SECRET FIELD TITLE");
    expect(body).not.toContain("SECRET OPTION LABEL");
    expect(call.options.timeout).toBe(10000);
    expect(call.options.cwd).toBe("/repo");
    expect(call.options.stdin).toBe("ignore");
    expect(call.options.stdout).toBe("ignore");
    expect(call.options.stderr).toBe("ignore");
    harness.dispose();
  });

  test("counts multiple fields without disclosing their content", async () => {
    const harness = createHarness();
    harness.stream.emit(
      questionEvent({
        fields: [
          { key: "a", type: "string", title: "A" },
          { key: "b", type: "string", title: "B" },
          { key: "c", type: "string", title: "C" },
        ],
      }),
    );
    await settle();
    expect(harness.spawned).toHaveLength(1);
    expect(bodyOf(harness.spawned[0])).toContain("3 fields");
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
    harness.stream.emit(questionEvent({ workspaceID: "ws_2" }));
    await settle();
    expect(harness.spawned).toHaveLength(0);
    harness.dispose();
  });

  test("notifies when both workspace ids are present and equal", async () => {
    const harness = createHarness({ location: { directory: "/repo", workspaceID: "ws_1" } });
    harness.stream.emit(questionEvent({ workspaceID: "ws_1" }));
    await settle();
    expect(harness.spawned).toHaveLength(1);
    harness.dispose();
  });

  test("resolves an absent event location from the form session", async () => {
    const harness = createHarness({ sessionLocation: { directory: "/repo", workspaceID: "ws_1" } });
    harness.stream.emit(questionEvent({ eventLocation: undefined, directory: null }));
    await settle();
    expect(harness.sessionGetInputs).toEqual([{ sessionID: "ses_1" }]);
    expect(harness.spawned).toHaveLength(1);
    harness.dispose();
  });

  test("skips an unresolvable location with a safe warning", async () => {
    const harness = createHarness({ sessionGetError: new Error("session unavailable") });
    harness.stream.emit(questionEvent({ eventLocation: undefined, directory: null }));
    await settle();
    expect(harness.spawned).toHaveLength(0);
    expect(harness.warnMessages.join("\n")).toContain("form location unknown");
    harness.dispose();
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
});

describe("question notifier failure handling (issue #1026 P1)", () => {
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

  test("survives a throwing spawn and still notifies a later question", async () => {
    let first = true;
    const harness = createHarness({
      spawn: () => {
        if (first) {
          first = false;
          throw new Error("spawn unavailable");
        }
        return { exited: Promise.resolve(0), kill() {} };
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

  test("survives a nonzero exit and still notifies a later question", async () => {
    const harness = createHarness({ spawn: () => ({ exited: Promise.resolve(2), kill() {} }) });
    harness.stream.emit(questionEvent({ id: "frm_nonzero" }));
    await settle();
    expect(harness.warnMessages.join("\n")).toContain("question notification delivery failed");
    harness.stream.emit(questionEvent({ id: "frm_next" }));
    await settle();
    expect(harness.spawned).toHaveLength(2);
    harness.dispose();
  });

  test("never blocks event handling on process completion", async () => {
    const harness = createHarness({
      spawn: () => ({ exited: new Promise(() => {}), kill() {} }),
    });
    harness.stream.emit(questionEvent({ id: "frm_hung_a" }));
    harness.stream.emit(questionEvent({ id: "frm_hung_b" }));
    await settle();
    // The first child never exits, yet the second question is still handled.
    expect(harness.spawned).toHaveLength(2);
    harness.dispose();
  });

  test("reports a subscription error safely", async () => {
    const harness = createHarness({ subscribeThrows: true });
    await settle();
    expect(harness.warnMessages.join("\n")).toContain("subscription failed");
    expect(() => harness.dispose()).not.toThrow();
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
    let killed = 0;
    const harness = createHarness({
      spawn: () => ({
        exited: new Promise(() => {}),
        kill() {
          killed += 1;
        },
      }),
    });
    harness.stream.emit(questionEvent({ id: "frm_owned" }));
    await settle();
    expect(harness.spawned).toHaveLength(1);
    harness.dispose();
    expect(killed).toBe(1);
    expect(harness.subscribeInfo.options.signal.aborted).toBe(true);
    harness.stream.emit(questionEvent({ id: "frm_after_dispose" }));
    await settle();
    expect(harness.spawned).toHaveLength(1);
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
      spawn: () => ({
        exited: Promise.reject(new Error(`exit rejected ${SENSITIVE_MARKER}\ninjected-line`)),
        kill() {},
      }),
    });
    harness.stream.emit(questionEvent({ id: "frm_reject_secret" }));
    await settle();
    const joined = harness.warnMessages.join("\n");
    expect(joined).toContain("question notification delivery failed");
    expect(joined).not.toContain(SENSITIVE_MARKER);
    expect(joined).not.toContain("injected-line");
    harness.dispose();
  });

  test("sanitizes a subscription error to a fixed failure category", async () => {
    const harness = createHarness({
      subscribeThrows: true,
      subscribeError: new Error(`subscribe failed ${SENSITIVE_MARKER}\ninjected-line`),
    });
    await settle();
    const joined = harness.warnMessages.join("\n");
    expect(joined).toContain("question notification subscription failed");
    expect(joined).not.toContain(SENSITIVE_MARKER);
    expect(joined).not.toContain("injected-line");
    harness.dispose();
  });

  test("sanitizes a session lookup error to a fixed failure category", async () => {
    const harness = createHarness({
      sessionGetError: new Error(`session lookup failed ${SENSITIVE_MARKER}\ninjected-line`),
    });
    harness.stream.emit(questionEvent({ eventLocation: undefined, directory: null }));
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
        throw new Error(`handling failed ${SENSITIVE_MARKER}\ninjected-line`);
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
});
