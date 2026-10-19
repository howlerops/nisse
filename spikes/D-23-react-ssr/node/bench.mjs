// Node baseline: same bundle, run in vm contexts so startup is comparable.
import fs from "node:fs";
import vm from "node:vm";
import { Worker, isMainThread, parentPort, workerData } from "node:worker_threads";

const [,, dist, durSec = "10", outHtml = "", bundle = "ssr.js"] = process.argv;
const src = fs.readFileSync(dist + "/" + bundle, "utf8");
const streamSrc = fs.readFileSync(dist + "/ssr-stream.js", "utf8");
const script = new vm.Script(src);

function newInstance() {
  const ctx = vm.createContext({ TextEncoder, TextDecoder, setTimeout, clearTimeout, queueMicrotask, MessageChannel, performance, ReadableStream });
  script.runInContext(ctx);
  return ctx;
}

function loop(ctx, ms) {
  const lat = [];
  const end = performance.now() + ms;
  while (performance.now() < end) {
    const t = performance.now();
    ctx.renderPage();
    lat.push(performance.now() - t);
  }
  return lat;
}

if (!isMainThread) {
  const ctx = newInstance();
  loop(ctx, workerData.ms / 5); // warm-up
  parentPort.postMessage(loop(ctx, workerData.ms).length);
} else {
  const pct = (a, p) => a[Math.min(a.length - 1, Math.floor(a.length * p))];
  // Startup: new context + run bundle, median of 10.
  const starts = [];
  for (let i = 0; i < 10; i++) { const t = performance.now(); newInstance(); starts.push(performance.now() - t); }
  starts.sort((a, b) => a - b);

  // Memory per instance: RSS growth over 8 instances after one render each.
  global.gc?.();
  const rss0 = process.memoryUsage().rss;
  const keep = [];
  for (let i = 0; i < 8; i++) { const c = newInstance(); c.renderPage(); keep.push(c); }
  global.gc?.();
  const perInst = (process.memoryUsage().rss - rss0) / 8;

  const ctx = newInstance();
  const html = ctx.renderPage();
  if (outHtml) fs.writeFileSync(outHtml, html);
  for (let i = 0; i < 200; i++) ctx.renderPage();
  const lat = loop(ctx, Number(durSec) * 1000).sort((a, b) => a - b);

  // Streaming with native ReadableStream.
  const sctx = vm.createContext({ TextEncoder, TextDecoder, setTimeout, clearTimeout, queueMicrotask, MessageChannel, performance, ReadableStream });
  new vm.Script(streamSrc).runInContext(sctx);
  sctx.startStream();
  await new Promise((r) => { const tick = () => (sctx.__stream ? r() : setTimeout(tick, 1)); tick(); });
  const streamOK = sctx.__stream.html === html;

  // Pool: 4 workers, each its own context.
  const ms = Number(durSec) * 1000;
  const counts = await Promise.all([0, 1, 2, 3].map(() => new Promise((res) => {
    const w = new Worker(new URL(import.meta.url), { workerData: { ms }, argv: [dist, durSec, "", bundle] });
    w.on("message", (n) => { res(n); w.terminate(); });
  })));
  const pool = counts.reduce((a, b) => a + b, 0) / (ms / 1000);

  console.log(JSON.stringify({
    engine: "node", bundle, version: process.version, htmlBytes: html.length,
    rendersPerSec: lat.length / Number(durSec), p50ms: pct(lat, 0.5), p99ms: pct(lat, 0.99),
    startupMs: starts[5], memPerInstanceMB: perInst / 1048576, pool4RendersPerSec: pool,
    stream: streamOK ? `ok (${sctx.__stream.chunks} chunks)` : "mismatch",
  }));
  process.exit(0);
}
