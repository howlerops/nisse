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
    g.TextDecoder = function () {};
    g.TextDecoder.prototype.decode = function (b) {
      if (!b) return "";
      let s = "", i = 0;
      while (i < b.length) {
        let c = b[i++];
        if (c >= 0xf0) c = ((c & 7) << 18) | ((b[i++] & 63) << 12) | ((b[i++] & 63) << 6) | (b[i++] & 63);
        else if (c >= 0xe0) c = ((c & 15) << 12) | ((b[i++] & 63) << 6) | (b[i++] & 63);
        else if (c >= 0xc0) c = ((c & 31) << 6) | (b[i++] & 63);
        s += String.fromCodePoint(c);
      }
      return s;
    };
  }
})(globalThis);
