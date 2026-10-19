// Engine-neutral entry: exposes globalThis.renderPage() returning HTML.
import React from "react";
import { renderToString } from "react-dom/server.browser";
import { App, makeData } from "./app.jsx";

const rows = makeData(globalThis.ROWS || 800);
const user = { name: "Ada" };
globalThis.renderPage = function () {
  return renderToString(<App rows={rows} user={user} />);
};
