// Streaming check: renderToReadableStream. The host calls startStream(),
// then drains timers and microtasks until globalThis.__stream is set.
import "./stream-polyfill.js";
import React from "react";
import { renderToReadableStream } from "react-dom/server.browser";
import { App, makeData } from "./app.jsx";

const rows = makeData(globalThis.ROWS || 800);
globalThis.startStream = function () {
  globalThis.__stream = null;
  renderToReadableStream(<App rows={rows} user={{ name: "Ada" }} />).then(async (stream) => {
    const reader = stream.getReader();
    const dec = new TextDecoder();
    let html = "", chunks = 0;
    for (;;) {
      const { value, done } = await reader.read();
      if (done) break;
      chunks++;
      html += dec.decode(value, { stream: true });
    }
    globalThis.__stream = { html, chunks };
  }).catch((e) => { globalThis.__stream = { html: "ERROR " + e, chunks: 0 }; });
};
// Host calls this repeatedly, letting the engine run microtasks between
// calls. Runs at most one timer per call. Returns "" until done, then
// "chunks\n" + html.
globalThis.pollStream = function () {
  if (!globalThis.__stream) globalThis.__runTimers();
  const s = globalThis.__stream;
  return s ? s.chunks + "\n" + s.html : "";
};
