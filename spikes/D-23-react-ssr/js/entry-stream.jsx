// Streaming check: renderToReadableStream, collecting chunks via a callback.
import "./stream-polyfill.js";
import React from "react";
import { renderToReadableStream } from "react-dom/server.browser";
import { App, makeData } from "./app.jsx";

const rows = makeData(globalThis.ROWS || 800);
globalThis.renderStream = function (done) {
  renderToReadableStream(<App rows={rows} user={{ name: "Ada" }} />).then(async (stream) => {
    const reader = stream.getReader();
    const dec = new TextDecoder();
    let html = "", chunks = 0;
    for (;;) {
      const { value, done: end } = await reader.read();
      if (end) break;
      chunks++;
      html += dec.decode(value, { stream: true });
    }
    done(html, chunks);
  }, (e) => done("ERROR " + e, 0));
};
