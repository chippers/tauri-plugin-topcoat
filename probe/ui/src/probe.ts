// The page half of the probe.
//
// Three steps, in order: provoke the behaviours only the server can see, answer
// the ones the page can see for itself, then wait for the report to stop
// changing and render it.
//
// Every type crossing to Rust comes from ./bindings.ts, which is generated from
// the Rust types. Nothing here describes a shape that lives over there.

import {
  commands,
  type Answer,
  type Clock,
  type Invoked,
  type Part,
  type Probe,
  type Sheet,
} from "./bindings";
import { describe, info, no, yes } from "./answer";

import "./probe.css";

// The provoked answers arrive whenever the webview gets to them - a form post,
// a followed navigation, a request from a foreign frame - so the page waits
// for the report to hold still rather than sleeping for a fixed guess.
const POLL_MS = 150;
const QUIET_MS = 1200;
const DEADLINE_MS = 8000;

/** What the page can answer on its own. `null` says the server owns this one. */
type PageProbe = (() => Answer | Promise<Answer>) | null;

/**
 * Every probe the Rust declares, and how the page answers it.
 *
 * Keyed by `Probe`, so this is exhaustive by construction: add a probe in
 * `src/report.rs`, regenerate, and TypeScript refuses to build until the page
 * either answers it or says `null` - the server's. Neither side can drift, and
 * neither can quietly forget.
 */
const PROBES: Record<Probe, PageProbe> = {
  "page-origin": () => info(location.origin),

  "user-agent": () => info(navigator.userAgent),

  "gzip-decoded": async () => {
    // The server gzips whatever is asked for, so the expected plaintext is
    // chosen here and nothing has to agree about a magic string. Read as bytes,
    // because an undecoded body is not text and printing it as text is noise.
    const expected = "decoded";
    const bytes = new Uint8Array(
      await (await fetch(query("/gzip", { text: expected }))).arrayBuffer(),
    );
    const body = new TextDecoder().decode(bytes);
    if (body === expected) {
      return yes("");
    }
    if (bytes[0] === 0x1f && bytes[1] === 0x8b) {
      return no("the body arrived still gzipped");
    }
    return no(`the body arrived as ${JSON.stringify(body.slice(0, 40))}`);
  },

  // A runtime URL, so the bundler leaves it alone and this stays what it is
  // asking about: a real module fetched over the scheme, not one inlined here.
  "es-module-loaded": async () => {
    const url = "/module.js";
    const module = (await import(/* @vite-ignore */ url)) as { loaded?: unknown };
    return module.loaded === true
      ? yes("")
      : no(`the module loaded but exported ${JSON.stringify(module.loaded)}`);
  },

  // Asked after `provoke` has pulled a stylesheet, two scripts, a module and
  // several fetches over the scheme, so an empty buffer means the buffer is
  // empty and not that nothing has been fetched yet.
  "resource-timing": () => {
    const entries = performance.getEntriesByType("resource");
    return entries.length > 0
      ? yes(`${entries.length} entries`)
      : no("the buffer is empty, so the page cannot time its own transport");
  },

  "fetch-post-body": async () => {
    const sent = "a fetch post body";
    const echoed = (await (await fetch("/echo", { method: "POST", body: sent })).json()) as {
      body: string;
    };
    return echoed.body === sent
      ? yes("")
      : no(`the server received ${JSON.stringify(echoed.body)}`);
  },

  "fetch-post-headers": null,
  "form-post-body": null,
  "form-post-headers": null,
  "set-cookie-returned": null,

  "document-cookie": () =>
    document.cookie ? info(document.cookie) : no("document.cookie is empty"),

  "fetch-follows-303": async () => {
    const token = "followed";
    const response = await fetch(query("/redirect/303", { via: "fetch", echo: token }));
    const body = (await response.text()).trim();
    return response.status === 200 && body === token
      ? yes("")
      : no(
          `status ${response.status}, redirected=${response.redirected}, ` +
            `body=${JSON.stringify(body.slice(0, 40))}`,
        );
  },

  "navigation-follows-303": null,

  "range-header-arrives": null,

  "csp-document-rendered": null,
  "csp-enforced": null,

  "ipc-invoke": async () => {
    const invoked = await invokeOnce();
    return invoked.echoed === TOKEN
      ? yes("")
      : no(`the command echoed ${JSON.stringify(invoked.echoed)}`);
  },

  "ipc-window-identity": async () => {
    const invoked = await invokeOnce();
    return invoked.window
      ? info(invoked.window)
      : no("the command could not say which window called it");
  },

  "ipc-invoke-in-frame": null,
  "ipc-invoke-under-csp": null,

  "local-storage": () => storage("localStorage"),

  "session-storage": () => storage("sessionStorage"),

  "indexed-db": async () => {
    try {
      await new Promise<void>((resolve, reject) => {
        const request = indexedDB.open("probe", 1);
        request.onsuccess = () => {
          request.result.close();
          resolve();
        };
        request.onerror = () => reject(request.error ?? new Error("open failed"));
        request.onblocked = () => reject(new Error("blocked"));
      });
      return yes("");
    } catch (error) {
      return no(describe(error));
    }
  },

  "foreign-document-ran": null,
  "foreign-post-delivered": null,
  "foreign-post-origin": null,
  "foreign-post-referer": null,
};

/** The route the header is rendered by, once a frame. Named in `route.rs`. */
const CLOCK = "/clock";

/** The token the page sends over IPC and expects back unchanged. */
const TOKEN = "a token from the page";

/** One `invoke`, shared by the two probes that read different parts of it. */
let invoked: Promise<Invoked> | null = null;
function invokeOnce(): Promise<Invoked> {
  invoked ??= commands.probeIpc(TOKEN);
  return invoked;
}

// A custom scheme can be an opaque origin, in which case this throws on access
// rather than coming back empty, and the page loses every client-side store at
// once.
function storage(name: "localStorage" | "sessionStorage"): Answer {
  try {
    window[name].setItem("probe", "1");
    const stored = window[name].getItem("probe") === "1";
    window[name].removeItem("probe");
    return stored ? yes("") : no("the value did not come back");
  } catch (error) {
    return no(describe(error));
  }
}

// Makes the things happen that only the server can measure. Nothing here is
// judged on this side.
async function provoke(): Promise<void> {
  await fetch("/cookies/set");
  await fetch("/cookies/echo");
  await fetch("/range", { headers: { Range: "bytes=0-99" } });

  frame(query("/redirect/303", { via: "nav", echo: "followed" }));
  frame("/csp");

  // Whether IPC survives being one frame down. The policy question is a
  // different one and is asked from a second window, because Tauri injects the
  // bridge into a window's main document and a frame would fail either way.
  frame(`/ipc/${"ipc-invoke-in-frame" satisfies Probe}/`);

  await foreignFrame();

  element<HTMLFormElement>("form").submit();
}

// A `data:` URL gives the document an opaque origin without needing a second
// server. Fetch requires an `Origin` on any method that is not GET or HEAD, so
// what arrives should name somebody else; the response is opaque to the frame
// that sent it, which is why the server reports this one.
async function foreignFrame(): Promise<void> {
  const template = await (await fetch("/foreign.html")).text();
  const foreign = template.replaceAll("__ORIGIN__", location.origin);
  frame(`data:text/html;charset=utf-8,${encodeURIComponent(foreign)}`);
}

async function run(): Promise<Partial<Record<Probe, Answer>>> {
  const answers: Partial<Record<Probe, Answer>> = {};

  for (const [id, probe] of Object.entries(PROBES) as [Probe, PageProbe][]) {
    if (probe === null) {
      continue;
    }
    try {
      answers[id] = await probe();
    } catch (error) {
      answers[id] = no(`the probe threw ${describe(error)}`);
    }
  }
  return answers;
}

async function settle(): Promise<Sheet> {
  const start = Date.now();
  let sheet = await report();
  let serialized = JSON.stringify(sheet);
  let changed = Date.now();

  while (Date.now() - changed < QUIET_MS && Date.now() - start < DEADLINE_MS) {
    await sleep(POLL_MS);
    sheet = await report();
    const current = JSON.stringify(sheet);
    if (current !== serialized) {
      serialized = current;
      changed = Date.now();
    }
  }
  return sheet;
}

const report = async (): Promise<Sheet> => (await fetch("/report")).json() as Promise<Sheet>;

const post = (path: string, body: unknown): Promise<Response> =>
  fetch(path, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(body),
  });

/**
 * A path with a query on it, where `null` is a key with no value.
 *
 * `URLSearchParams` is the half of the grammar `form_urlencoded` reads back in
 * `route.rs`, so neither end has to know what the other escapes.
 */
const query = (path: string, params: Record<string, string | number | null>): string => {
  const search = new URLSearchParams();
  for (const [name, value] of Object.entries(params)) {
    search.set(name, value === null ? "" : String(value));
  }
  const encoded = search.toString();
  return encoded ? `${path}?${encoded}` : path;
};

const sleep = (ms: number): Promise<void> => new Promise((resolve) => setTimeout(resolve, ms));

const nextFrame = (): Promise<void> =>
  new Promise((resolve) => requestAnimationFrame(() => resolve()));

/**
 * Paints the header, once an animation frame, forever.
 *
 * The page contributes only the timing, which rides up on the following frame
 * because nothing can time itself. `README.md` says what the clock asserts.
 */
async function clock(): Promise<never> {
  const face = element<HTMLOutputElement>("clock");
  const coarse = document.createElement("span");
  const fine = document.createElement("span");
  fine.className = "fine";
  face.replaceChildren(coarse, fine);

  const stats = element<HTMLTableElement>("stats");
  const breakdown = element<HTMLTableElement>("breakdown");
  await burst();

  // The previous frame's timing, because nothing can time itself. Empty only
  // on the first pass, where the server reads no numbers and says so.
  let timed: Record<string, number> = {};
  let drawn = "";

  for (;;) {
    await nextFrame();

    const start = performance.now();
    const response = await fetch(query(CLOCK, timed));
    const head = performance.now();
    const shown = (await response.json()) as Clock;
    const done = performance.now();
    timed = { head: micros(head - start), body: micros(done - head) };

    coarse.textContent = shown.coarse;
    fine.textContent = shown.fine;

    // Only when they change, which is most frames they do not: the means
    // behind these settle, and rebuilding two tables sixty times a second to
    // write the same words into them is work nobody asked for.
    const rendered = JSON.stringify([shown.stats, shown.breakdown]);
    if (rendered !== drawn) {
      drawn = rendered;
      parts(stats, shown.stats);
      parts(breakdown, shown.breakdown);
    }
  }
}

/** Fills a table with readings the server has already formatted. */
function parts(table: HTMLTableElement, readings: Part[]): void {
  table.textContent = "";
  for (const reading of readings) {
    const line = table.insertRow();
    line.insertCell().textContent = reading.label;
    line.insertCell().textContent = reading.value;
    line.insertCell().textContent = reading.note;
  }
}

const micros = (ms: number): number => Math.round(ms * 1e3);

/** How many back-to-back round trips the transport is timed with. */
const BURST = 200;

/**
 * What the transport costs with nothing in its way, timed by hand.
 *
 * By hand because Resource Timing is empty over a custom scheme, and outside
 * the frames that draw the clock because theirs is mostly the frame.
 *
 * Called twice: once at the start so the header says something, and again once
 * the probes have stopped competing for the machine.
 */
async function burst(): Promise<void> {
  let head = 0;
  let body = 0;

  for (let index = 0; index < BURST; index += 1) {
    const start = performance.now();
    // `measuring` keeps these out of the frame count. They are round trips,
    // but they are not frames, and the header claims one a frame. A bare key,
    // which the server reads with the same urlencoded grammar as this end.
    const response = await fetch(query(CLOCK, { measuring: null }));
    const arrived = performance.now();
    await response.json();
    head += arrived - start;
    body += performance.now() - arrived;
  }

  // Reported on a request of its own - also not a frame - so the figure
  // lands the moment it exists rather than waiting for a frame to carry it.
  await fetch(query(CLOCK, { measuring: null, head: micros(head), body: micros(body), of: BURST }));
}

function render(sheet: Sheet): void {
  // Counted on the server, so the pane title and the terminal footer cannot
  // reach different conclusions about the same rows.
  element("tally").textContent = sheet.tally;
  element("news").textContent = sheet.news;

  const body = element<HTMLTableSectionElement>("rows");
  body.textContent = "";

  for (const row of sheet.rows) {
    const line = body.insertRow();
    line.insertCell().textContent = row.label;

    const verdict = line.insertCell();
    verdict.className = row.verdict;
    verdict.textContent = row.verdict;

    // The word we predicted, in brackets when it is not what happened - the
    // same mark the terminal makes, because the colour beside it is not
    // something every reader has.
    const news = row.expected !== row.verdict;
    const expected = line.insertCell();
    expected.className = news ? "expected news" : "expected";
    expected.textContent = news ? `[${row.expected}]` : row.expected;

    const detail = line.insertCell();
    detail.className = "detail";
    detail.textContent = row.detail;
  }
}

function status(message: string, running: boolean): void {
  const line = element("status");
  line.textContent = message;
  line.className = running ? "running" : "";
}

function frame(src: string): void {
  const iframe = document.createElement("iframe");
  iframe.src = src;
  element("frames").appendChild(iframe);
}

function element<T extends HTMLElement = HTMLElement>(id: string): T {
  const found = document.getElementById(id);
  if (found === null) {
    throw new Error(`index.html has no #${id}`);
  }
  return found as T;
}

async function main(): Promise<void> {
  await provoke();
  await post("/answers", await run());

  status("waiting for the server to stop learning things...", true);
  render(await settle());
  status("done. the same table is on stdout.", false);

  // Now the probes are done competing for the machine, and before `--exit`
  // takes the process away.
  await burst();
  await post("/done", {});
}

// Started alongside the run and never awaited: the report finishes and the
// clock does not.
clock().catch((error: unknown) => {
  // Said out loud, not swallowed. A clock that stops is this page's loudest
  // signal that the transport did, and the one boring cause - `--exit` taking
  // the server away once it has reported - can say so itself.
  status(`the clock stopped: ${describe(error)}`, false);
});

main().catch((error: unknown) => {
  status(`the probe did not finish: ${describe(error)}`, false);
});
