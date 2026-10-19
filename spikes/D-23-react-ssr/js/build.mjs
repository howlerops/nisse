// Bundles the benchmark for every engine.
//   ssr.js            stock React 19 renderToString
//   ssr-join.js       same, with React's `result += chunk` accumulation in
//                     renderToString replaced by an array join (one-line patch
//                     for engines without rope strings, such as goja)
//   ssr-stream.js     renderToReadableStream with a ReadableStream polyfill
import * as esbuild from "esbuild";
import fs from "node:fs";

const prelude = fs.readFileSync("src/prelude.js", "utf8");

const joinPatch = {
  name: "join-patch",
  setup(build) {
    build.onLoad({ filter: /react-dom-server-legacy\.browser\.production\.js$/ }, (args) => {
      let src = fs.readFileSync(args.path, "utf8");
      const edits = [
        ['result = "",\n    readyToStream', 'result = "",\n    parts = [],\n    readyToStream'],
        ["null !== chunk && (result += chunk);", "null !== chunk && parts.push(chunk);"],
        ["if (!readyToStream) throw Error(formatProdErrorMessage(426));\n  return result;",
         'if (!readyToStream) throw Error(formatProdErrorMessage(426));\n  return parts.join("");'],
      ];
      for (const [a, b] of edits) {
        if (!src.includes(a)) throw new Error("join patch no longer applies: " + a);
        src = src.replace(a, b);
      }
      return { contents: src, loader: "js" };
    });
  },
};

async function bundle(entry, out, plugins = []) {
  await esbuild.build({
    entryPoints: [entry], bundle: true, preserveSymlinks: true, platform: "browser",
    format: "iife", minify: true, define: { "process.env.NODE_ENV": '"production"' },
    outfile: "dist/tmp.js", plugins, logLevel: "warning",
  });
  fs.writeFileSync(out, prelude + fs.readFileSync("dist/tmp.js", "utf8"));
  fs.rmSync("dist/tmp.js");
}

fs.mkdirSync("dist", { recursive: true });
await bundle("src/entry.jsx", "dist/ssr.js");
await bundle("src/entry.jsx", "dist/ssr-join.js", [joinPatch]);
await bundle("src/entry-stream.jsx", "dist/ssr-stream.js");
for (const f of ["ssr.js", "ssr-join.js", "ssr-stream.js"]) console.log(f, fs.statSync("dist/" + f).size);
