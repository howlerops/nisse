# Nisse on the phone

A [Native Shell](https://nsh.dev) app: `App.tsx` is the screens, `ext/nisse-native` is the
Rust module they import. It shows what the engine's store says and can do one thing: engage
the kill switch. It never talks to the broker and holds no broker credential. Design:
[`docs/phases/phase-8.md`](../docs/phases/phase-8.md). Running it on a phone:
[`docs/runbooks/phone-ui.md`](../docs/runbooks/phone-ui.md).

## Tests

```
cd ui/ext/nisse-native
TEST_DATABASE_URL=postgres://user@host/postgres cargo test
```

These bundle `App.tsx`, run it headless on QuickJS, and drive it by pressing and typing. The
module and the database are real: the app connects to a scratch Postgres as the role
`nisse_ui`. No device and no `nsh` command are needed, and CI runs them.

## Not yet done

- `nsh check` (TypeScript types) and any device build. See the runbook.
- The Proposals screen's call to GitHub is not exercised by the tests.
