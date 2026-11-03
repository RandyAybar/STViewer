// Ad-hoc access to a running STViewer page over the WebView2 DevTools protocol (Windows).
// Start the app with WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS=--remote-debugging-port=9337 and,
// for an isolated library, STVIEWER_DATA_DIR / WEBVIEW2_USER_DATA_FOLDER pointing at a temp dir.
//   node app/scripts/cdp-eval.mjs "<expression>" [port]   → prints the JSON result
//   node app/scripts/cdp-eval.mjs --shot <file.png> [port] → saves a screenshot
import { writeFileSync } from "node:fs";

const shot = process.argv[2] === "--shot";
const [arg, port = "9337"] = process.argv.slice(shot ? 3 : 2);
const list = await (await fetch(`http://127.0.0.1:${port}/json`)).json();
const page = list.find((t) => t.type === "page");
const ws = new WebSocket(page.webSocketDebuggerUrl);
await new Promise((r) => (ws.onopen = r));
ws.onmessage = (ev) => {
  const msg = JSON.parse(ev.data);
  if (msg.id !== 1) return;
  const r = msg.result;
  if (shot) {
    writeFileSync(arg, Buffer.from(r.data, "base64"));
    console.log(`saved ${arg}`);
  } else if (r?.exceptionDetails) {
    console.error(r.exceptionDetails.exception?.description ?? JSON.stringify(r.exceptionDetails));
  } else {
    console.log(JSON.stringify(r?.result?.value ?? r ?? msg.error, null, 2));
  }
  ws.close();
};
ws.send(
  JSON.stringify(
    shot
      ? { id: 1, method: "Page.captureScreenshot", params: { format: "png" } }
      : { id: 1, method: "Runtime.evaluate", params: { expression: arg, awaitPromise: true, returnByValue: true } },
  ),
);
