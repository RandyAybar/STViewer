// Regenerates the README screenshots in docs/screenshots/ from the committed fixtures.
//
// Windows only (drives the WebView2 DevTools protocol). Build the app first:
//   cd app && npm run build && cd ..
//   set CARGO_TARGET_DIR=target/ui
//   cargo build -p stviewer-app --features custom-protocol --profile release-fast
// then from the repository root:
//   node app/scripts/screenshots.mjs [--exe target/ui/release-fast/stviewer-app.exe] [--out docs/screenshots] [--port 9337]
//
// The script starts the app on a fresh temporary library (STVIEWER_DATA_DIR) and WebView2
// profile, adds testdata/app/demo.mbox and the testdata/library fixtures, waits for
// indexing, saves the shots, then stops the app and deletes the temporary directory.
import { spawn } from "node:child_process";
import { mkdtempSync, mkdirSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

const arg = (name, dflt) => {
  const i = process.argv.indexOf(`--${name}`);
  return i > 0 ? process.argv[i + 1] : dflt;
};
const exe = resolve(arg("exe", "target/ui/release-fast/stviewer-app.exe"));
const outDir = resolve(arg("out", "docs/screenshots"));
const port = Number(arg("port", "9337"));
const sources = [
  resolve("testdata/app/demo.mbox"),
  resolve("testdata/library/archive.mbox"),
  resolve("testdata/library/eml"),
];
const WIDTH = 1440;
const HEIGHT = 900;

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

const tmp = mkdtempSync(join(tmpdir(), "stv-shots-"));
const app = spawn(exe, [], {
  env: {
    ...process.env,
    STVIEWER_DATA_DIR: join(tmp, "data"),
    WEBVIEW2_USER_DATA_FOLDER: join(tmp, "webview"),
    WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: `--remote-debugging-port=${port}`,
  },
  stdio: "ignore",
});

let ws;
try {
  ws = await connect();
  await run();
} finally {
  ws?.close();
  app.kill();
  await sleep(1000); // let WebView2 release its profile files
  rmSync(tmp, { recursive: true, force: true, maxRetries: 5, retryDelay: 500 });
}

async function connect() {
  for (let i = 0; i < 120; i++) {
    try {
      const list = await (await fetch(`http://127.0.0.1:${port}/json`)).json();
      const page = list.find((t) => t.type === "page");
      if (page) {
        const socket = new WebSocket(page.webSocketDebuggerUrl);
        await new Promise((r, j) => {
          socket.onopen = r;
          socket.onerror = j;
        });
        return socket;
      }
    } catch {
      // DevTools endpoint not up yet.
    }
    await sleep(500);
  }
  throw new Error(`no STViewer page on DevTools port ${port}`);
}

async function run() {
  let nextId = 0;
  const pending = new Map();
  ws.onmessage = (ev) => {
    const msg = JSON.parse(ev.data);
    const p = msg.id && pending.get(msg.id);
    if (!p) return;
    pending.delete(msg.id);
    if (msg.error) p.reject(new Error(JSON.stringify(msg.error)));
    else p.resolve(msg.result);
  };
  const send = (method, params = {}) =>
    new Promise((resolve, reject) => {
      const id = ++nextId;
      pending.set(id, { resolve, reject });
      ws.send(JSON.stringify({ id, method, params }));
    });
  const evaluate = async (expr) => {
    const r = await send("Runtime.evaluate", { expression: expr, awaitPromise: true, returnByValue: true });
    if (r.exceptionDetails) throw new Error(`${expr}: ${JSON.stringify(r.exceptionDetails)}`);
    return r.result.value;
  };
  const waitFor = async (expr, ms = 20000) => {
    const t0 = Date.now();
    while (Date.now() - t0 < ms) {
      if (await evaluate(`!!(${expr})`)) return;
      await sleep(150);
    }
    throw new Error(`timed out waiting for ${expr}`);
  };
  const invoke = (cmd, args) =>
    evaluate(`window.__TAURI_INTERNALS__.invoke(${JSON.stringify(cmd)}, ${JSON.stringify(args)})`);
  const click = (selector, text = "") =>
    evaluate(`(() => {
      const el = [...document.querySelectorAll(${JSON.stringify(selector)})].find((e) => e.textContent.includes(${JSON.stringify(text)}));
      el?.click();
      return !!el;
    })()`);
  const shot = async (name) => {
    await sleep(500);
    const { data } = await send("Page.captureScreenshot", { format: "png" });
    const path = join(outDir, `${name}.png`);
    writeFileSync(path, Buffer.from(data, "base64"));
    console.log(`saved ${path}`);
  };

  mkdirSync(outDir, { recursive: true });
  await send("Page.enable");
  await send("Runtime.enable");
  await send("Emulation.setDeviceMetricsOverride", {
    width: WIDTH,
    height: HEIGHT,
    deviceScaleFactor: 1,
    mobile: false,
  });
  await send("Emulation.setEmulatedMedia", { features: [{ name: "prefers-color-scheme", value: "light" }] });

  // Fresh library: add the fixtures through the same command as drag & drop, then reload.
  await waitFor(`window.__TAURI_INTERNALS__ && document.querySelector('.welcome')`);
  const added = await invoke("add_paths", { paths: sources });
  if (added.errors.length) throw new Error(`add_paths: ${JSON.stringify(added.errors)}`);
  for (let i = 0; i < 240; i++) {
    const p = await invoke("index_progress", {});
    if (p.length === sources.length && p.every((x) => x.done)) break;
    await sleep(500);
  }
  await send("Page.reload");

  // Folder list + an HTML message (inline cid: logo, blocked tracking pixel).
  await waitFor(`document.querySelectorAll('.list .row').length > 3`);
  // Some "Hiring" mails are plain text; take the first one that renders the HTML body.
  const hiring = `[...document.querySelectorAll('.list .row')].filter((r) => r.textContent.includes('Hiring'))`;
  const rows = await evaluate(`${hiring}.length`);
  for (let k = 0; k < rows; k++) {
    await evaluate(`${hiring}[${k}].click()`);
    await sleep(400);
    if (await evaluate(`!!document.querySelector('iframe.html')?.contentDocument?.querySelector('img[alt=logo]')`))
      break;
  }
  await waitFor(`document.querySelector('iframe.html')?.contentDocument?.querySelector('img[alt=logo]')?.complete`);
  await shot("main");

  // Conversation of a reply thread, every card expanded.
  await click(".list .row", "Re: Offsite planning");
  await waitFor(`document.querySelector('.reader h1')?.textContent.includes('Offsite')`);
  await click(".tabs button", "Conversation");
  await waitFor(`document.querySelector('.timeline .card')`);
  await click(".bar .link", "Expand all");
  await sleep(800);
  await shot("conversation");

  // Headers: delivery path and authentication results.
  await click(".tabs button", "Headers");
  await waitFor(`document.querySelector('.hv')`);
  await shot("headers");
  await click(".tabs button", "Message");

  // Full-text search with facets.
  await evaluate(`(() => {
    const i = document.querySelector('.searchbar input');
    i.value = 'budget';
    i.dispatchEvent(new Event('input', { bubbles: true }));
    i.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }));
  })()`);
  await waitFor(`document.querySelector('.facets .summary b')`);
  await click(".list .row", "budget");
  await waitFor(`document.querySelector('.reader h1')`);
  await shot("search");
  await evaluate(`document.querySelector('.searchbar .clear')?.click()`);

  // Attachments of the demo mailbox, icons view, first image previewed.
  await click(".row.folder", "Attachments");
  await waitFor(`document.querySelector('.atts .tile, .atts .row')`);
  await click(".atts .seg button[title=Icons]");
  await waitFor(`document.querySelector('.atts .tile img')`);
  await evaluate(`[...document.querySelectorAll('.atts .tile')].find((t) => t.querySelector('img'))?.click()`);
  await shot("attachments");

  // Settings: agent access (MCP) with the server off.
  await evaluate(`document.querySelector('.topbar button[aria-label=Settings]').click()`);
  await waitFor(`document.querySelector('.dialog')`);
  await shot("agent-access");
  await evaluate(`document.querySelector('.dialog button[aria-label=Close]').click()`);

  // Dark theme, main view.
  await send("Emulation.setEmulatedMedia", { features: [{ name: "prefers-color-scheme", value: "dark" }] });
  await click(".row.source", "demo");
  await waitFor(`document.querySelectorAll('.list .row').length > 3`);
  await click(".list .row", "Quarterly budget review");
  await waitFor(`document.querySelector('.reader h1')?.textContent.includes('budget')`);
  await shot("dark");
}
