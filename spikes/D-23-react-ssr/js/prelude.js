// Minimal host shims for engines without Web/Node globals (goja, QuickJS).
// Everything is defined only if missing, so Node keeps its native versions.
(function (g) {
  if (typeof g.window === "undefined") g.window = undefined;
  if (typeof g.process === "undefined") g.process = { env: { NODE_ENV: "production" } };
  if (typeof g.performance === "undefined") g.performance = { now: () => Date.now() };
  if (typeof g.queueMicrotask === "undefined") g.queueMicrotask = (f) => Promise.resolve().then(f);

  // Timers live in a JS queue that the host drains with __runTimers().
  if (typeof g.setTimeout === "undefined") {
    let next = 1;
    const timers = [];
    g.setTimeout = (fn, ms, ...args) => { const id = next++; timers.push({ id, fn, args }); return id; };
    g.clearTimeout = (id) => { const i = timers.findIndex((t) => t.id === id); if (i >= 0) timers.splice(i, 1); };
    g.setImmediate = (fn, ...args) => g.setTimeout(fn, 0, ...args);
    g.__runTimers = () => {
      if (timers.length === 0) return false;
      const t = timers.shift();
      t.fn(...t.args);
      return true;
    };
  } else {
    g.__runTimers = () => false;
  }

  if (typeof g.MessageChannel === "undefined") {
    g.MessageChannel = function () {
      const port1 = { onmessage: null };
      const port2 = { postMessage: (data) => g.setTimeout(() => port1.onmessage && port1.onmessage({ data }), 0) };
      this.port1 = port1;
      this.port2 = port2;
    };
  }

  if (typeof g.TextEncoder === "undefined") {
    g.TextEncoder = function () {};
    g.TextEncoder.prototype.encode = function (s) {
      s = String(s === undefined ? "" : s);
      const out = [];
      for (let i = 0; i < s.length; i++) {
        let c = s.charCodeAt(i);
        if (c >= 0xd800 && c <= 0xdbff && i + 1 < s.length) {
          const d = s.charCodeAt(i + 1);
          if (d >= 0xdc00 && d <= 0xdfff) { c = 0x10000 + ((c - 0xd800) << 10) + (d - 0xdc00); i++; }
        }
        if (c < 0x80) out.push(c);
        else if (c < 0x800) out.push(0xc0 | (c >> 6), 0x80 | (c & 63));
        else if (c < 0x10000) out.push(0xe0 | (c >> 12), 0x80 | ((c >> 6) & 63), 0x80 | (c & 63));
        else out.push(0xf0 | (c >> 18), 0x80 | ((c >> 12) & 63), 0x80 | ((c >> 6) & 63), 0x80 | (c & 63));
      }
      return new Uint8Array(out);
    };
    g.TextEncoder.prototype.encodeInto = function (s, dest) {
      const b = this.encode(s); dest.set(b); return { read: s.length, written: b.length };
    };
  }
  if (typeof g.TextDecoder === "undefined") {
    // UTF-8 decoder; with { stream: true } an incomplete trailing sequence
    // is kept for the next call, as the real TextDecoder does.
    g.TextDecoder = function () { this._pending = []; };
    g.TextDecoder.prototype.decode = function (input, opts) {
      const b = this._pending.concat(Array.from(input || []));
      this._pending = [];
      const stream = !!(opts && opts.stream);
      let s = "", i = 0;
      while (i < b.length) {
        const c0 = b[i];
        const need = c0 >= 0xf0 ? 4 : c0 >= 0xe0 ? 3 : c0 >= 0xc0 ? 2 : 1;
        if (i + need > b.length && stream) { this._pending = b.slice(i); break; }
        let c = c0;
        if (need === 4) c = ((c0 & 7) << 18) | ((b[i + 1] & 63) << 12) | ((b[i + 2] & 63) << 6) | (b[i + 3] & 63);
        else if (need === 3) c = ((c0 & 15) << 12) | ((b[i + 1] & 63) << 6) | (b[i + 2] & 63);
        else if (need === 2) c = ((c0 & 31) << 6) | (b[i + 1] & 63);
        s += String.fromCodePoint(c);
        i += need;
      }
      return s;
    };
  }
})(globalThis);
