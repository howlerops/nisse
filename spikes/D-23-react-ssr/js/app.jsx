// Benchmark app: context, hooks, a large table, conditional rendering.
import React, { createContext, useContext, useId, useMemo, useState } from "react";

export const ThemeContext = createContext({ name: "light", accent: "#0a7" });
export const UserContext = createContext(null);

// Deterministic data so every engine and the Go baseline render the same HTML.
export function makeData(n) {
  let seed = 42;
  const rand = () => ((seed = (seed * 1103515245 + 12345) % 2147483648) / 2147483648);
  const statuses = ["active", "paused", "archived"];
  const rows = [];
  for (let i = 0; i < n; i++) {
    const r = rand();
    rows.push({
      id: i + 1,
      name: "Customer " + (i + 1),
      email: "user" + (i + 1) + "@example.com",
      status: statuses[Math.floor(r * 3)],
      mrr: Math.floor(rand() * 100000) / 100,
      tags: i % 7 === 0 ? ["vip", "beta"] : i % 3 === 0 ? ["beta"] : [],
      note: i % 11 === 0 ? "Needs <review> & \"follow-up\"" : null,
    });
  }
  return rows;
}

function Badge({ status }) {
  const theme = useContext(ThemeContext);
  const cls = "badge badge-" + status;
  return <span className={cls} style={{ borderColor: theme.accent }}>{status}</span>;
}

function Row({ row, index }) {
  const [expanded] = useState(index % 50 === 0);
  return (
    <tr className={index % 2 === 0 ? "even" : "odd"} data-id={row.id}>
      <td>{row.id}</td>
      <td>
        <a href={"/customers/" + row.id}>{row.name}</a>
        {row.tags.length > 0 && (
          <ul className="tags">
            {row.tags.map((t) => <li key={t}>{t}</li>)}
          </ul>
        )}
      </td>
      <td>{row.email}</td>
      <td><Badge status={row.status} /></td>
      <td className="num">${row.mrr.toFixed(2)}</td>
      <td>{row.note ? <em>{row.note}</em> : "—"}{expanded ? <details open><summary>More</summary>Row {index} details</details> : null}</td>
    </tr>
  );
}

function Filter() {
  const id = useId();
  const [q] = useState("");
  return (
    <form className="filter">
      <label htmlFor={id}>Search</label>
      <input id={id} type="search" defaultValue={q} placeholder="Name or email" />
      <input type="checkbox" defaultChecked={true} aria-label="Active only" />
    </form>
  );
}

function Summary({ rows }) {
  const totals = useMemo(() => {
    const t = { active: 0, paused: 0, archived: 0, mrr: 0 };
    for (const r of rows) { t[r.status]++; t.mrr += r.mrr; }
    return t;
  }, [rows]);
  return (
    <section className="summary">
      <h2>Summary</h2>
      <p>Active: {totals.active}, paused: {totals.paused}, archived: {totals.archived}</p>
      <p>Total MRR: ${totals.mrr.toFixed(2)}</p>
    </section>
  );
}

function Header() {
  const user = useContext(UserContext);
  return (
    <header>
      <h1>Customers</h1>
      {user ? <span className="user">Signed in as {user.name}</span> : <a href="/login">Sign in</a>}
    </header>
  );
}

export function App({ rows, user }) {
  return (
    <ThemeContext.Provider value={{ name: "dark", accent: "#f60" }}>
      <UserContext.Provider value={user}>
        <div className="app">
          <Header />
          <Filter />
          <Summary rows={rows} />
          <table className="customers">
            <thead><tr><th>ID</th><th>Name</th><th>Email</th><th>Status</th><th>MRR</th><th>Notes</th></tr></thead>
            <tbody>{rows.map((row, i) => <Row key={row.id} row={row} index={i} />)}</tbody>
          </table>
          <footer>{rows.length} customers</footer>
        </div>
      </UserContext.Provider>
    </ThemeContext.Provider>
  );
}
