# research

Everything a research agent may write lives here, and nothing else does.

| Path | What | Who writes |
|---|---|---|
| `configs/<name>.json` | Strategy configs (format in `schema/strategy-config.schema.json`) | agents, by PR |
| `notes/` | The note behind each config, and daily notes | agents, by PR |
| `labels/` | Post-mortem labels | agents, by PR |
| `signed/` | Signed manifests for merged configs | the signing job, never an agent |
| `agents/` | The instructions each scheduled run is given | people |
| `schema/` | The config format | people |

A pull request that touches `configs/`, `notes/` or `labels/` may touch nothing else, may
change at most one config, and must leave 30 days since the last change to the same strategy
family. CI checks this (`nisse-proposal-check`); a person merges. Design:
`docs/phases/phase-7.md`.

Nothing here is investment advice.
