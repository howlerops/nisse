// Nisse on the phone: what the engine's store says, and one action, engaging the kill switch.
//
// The screens read through the app's Rust module (`ext/nisse-native`), which connects to
// Postgres as a role that can read and can engage the kill switch, and nothing else. The app
// never talks to the broker and holds no broker credential. Design: docs/phases/phase-8.md.
//
// One rule runs through every screen: the app shows a state only when it has just read it.
// When a read fails it says "cannot confirm" and never falls back to what it saw before.
import { cleanup, type JSX, mount, openUrl, resource, Scroll, signal, Storage, Text, View } from "nsh";
import { Button as BaseButton, Input } from "nsh/base";
import { Window } from "nsh/desktop";
import { Nisse } from "nisse-native";

/** Where the connection string is kept in the app's storage. */
const URL_KEY = "nisse.database_url";
/** How often the open screen re-reads the store, in milliseconds. */
const REFRESH_MS = 30_000;
/** A heartbeat older than this (seconds) is shown as late. The engine ticks every two minutes. */
const HEARTBEAT_LATE_SECONDS = 300;
/** The repository whose open pull requests the Proposals screen lists. */
const PULLS_URL = "https://api.github.com/repos/howlerops/nisse/pulls?state=open&per_page=30";

const RED = "#b42318";
const GREEN = "#067647";
const AMBER = "#b54708";
const INK = "#101828";
const MUTED = "#475467";
const PAPER = "#ffffff";
const WASH = "#f2f4f7";

type Tab = "status" | "positions" | "gates" | "proposals" | "kill" | "settings";
const TABS: { id: Tab; title: string }[] = [
  { id: "status", title: "Status" },
  { id: "positions", title: "Positions" },
  { id: "gates", title: "Gates" },
  { id: "proposals", title: "Proposals" },
  { id: "kill", title: "Kill" },
  { id: "settings", title: "Settings" },
];

/** What to do when the app cannot confirm the engine's state. */
const OUT_OF_BAND =
  "If you need to stop trading now, do it at the broker: log in to Alpaca, cancel all open orders, " +
  "and regenerate the API keys (docs/runbooks/kill-switch.md).";

/** A pull request as GitHub's API lists it (the fields the app uses). */
type Pull = {
  number: number;
  title: string;
  html_url: string;
  user: { login: string } | null;
  head: { repo: { full_name: string } | null };
  base: { repo: { full_name: string } };
};

/** A press target with a label. */
function Button(props: { title: string; tone?: "plain" | "primary" | "danger"; disabled?: boolean; onPress: () => void }) {
  const background = () => (props.tone === "danger" ? RED : props.tone === "primary" ? INK : WASH);
  const ink = () => (props.tone === "danger" || props.tone === "primary" ? PAPER : INK);
  return (
    <BaseButton
      onPress={props.onPress}
      disabled={props.disabled}
      style={{ backgroundColor: background(), borderRadius: 8, paddingHorizontal: 14, paddingVertical: 10 }}
      activeStyle={{ opacity: 0.7 }}
      disabledStyle={{ opacity: 0.4 }}
    >
      <Text style={{ fontSize: 15, fontWeight: "600", color: ink() }}>{props.title}</Text>
    </BaseButton>
  );
}

/** A titled group of lines. */
function Card(props: { title: string; children: JSX.Element }) {
  return (
    <View style={{ gap: 8, padding: 16, borderRadius: 12, backgroundColor: WASH }}>
      <Text style={{ fontSize: 13, fontWeight: "600", color: MUTED }}>{props.title}</Text>
      {props.children}
    </View>
  );
}

/** A line of text. */
function Line(props: { children: string; color?: string; strong?: boolean }) {
  return (
    <Text style={{ fontSize: 15, color: props.color ?? INK, fontWeight: props.strong ? "600" : "normal" }}>
      {props.children}
    </Text>
  );
}

/** Shown in place of any state the app could not read just now. */
function CannotConfirm(props: { what: string; error: unknown }) {
  return (
    <Card title="CANNOT CONFIRM">
      <Line color={RED} strong>{`Cannot confirm ${props.what}.`}</Line>
      <Line color={MUTED}>{String(props.error)}</Line>
      <Line color={MUTED}>{OUT_OF_BAND}</Line>
    </Card>
  );
}

/** Shown on every data screen until a connection is saved. */
function NotConnected() {
  return (
    <Card title="NOT CONNECTED">
      <Line>Add the store's connection in Settings.</Line>
    </Card>
  );
}

/** Re-reads `read` every `REFRESH_MS` while the calling screen is on show. */
function keepFresh(read: () => void) {
  const id = setInterval(() => read(), REFRESH_MS);
  cleanup(() => clearInterval(id));
}

function StatusScreen(props: { url: () => string | null }) {
  const status = resource(props.url, (url) => Nisse.status(url));
  keepFresh(status.refetch);
  const fresh = () => status.state() === "ready" || status.state() === "refreshing";
  const heartbeat = () => {
    const age = status()?.heartbeatAgeSeconds;
    if (age === null || age === undefined) return { text: "Heartbeat: never", color: AMBER };
    const late = age > HEARTBEAT_LATE_SECONDS;
    return { text: `Heartbeat: ${Math.round(age)} s ago${late ? " (late)" : ""}`, color: late ? AMBER : INK };
  };
  return (
    <View style={{ gap: 16 }}>
      <If when={!props.url()}>
        <NotConnected />
      </If>
      <If when={status.error()}>
        <CannotConfirm what="the engine's state" error={status.error()} />
      </If>
      <If when={fresh() && status()}>
        <Card title="ENGINE">
          <Line color={status()?.killSwitch ? RED : GREEN} strong>
            {status()?.killSwitch ? "Kill switch: ENGAGED" : "Kill switch: off"}
          </Line>
          <Line>{`Drawdown mode: ${status()?.drawdownMode ?? ""}`}</Line>
          <Line>{`Equity peak: ${status()?.equityPeak ?? "none yet"}`}</Line>
          <Line color={heartbeat().color}>{heartbeat().text}</Line>
          <Line color={status()?.rebalanceOwed ? AMBER : INK}>
            {status()?.rebalanceOwed ? "Rebalance owed: yes" : "Rebalance owed: no"}
          </Line>
          <Line color={MUTED}>{`Limits: ${status()?.limitsHash ?? "none recorded"}`}</Line>
        </Card>
      </If>
      <Button title="Refresh" onPress={() => status.refetch()} />
    </View>
  );
}

function PositionsScreen(props: { url: () => string | null }) {
  const positions = resource(props.url, (url) => Nisse.positions(url));
  const orders = resource(props.url, (url) => Nisse.orders(url));
  keepFresh(() => {
    positions.refetch();
    orders.refetch();
  });
  return (
    <View style={{ gap: 16 }}>
      <If when={!props.url()}>
        <NotConnected />
      </If>
      <If when={positions.error() ?? orders.error()}>
        <CannotConfirm what="positions and orders" error={positions.error() ?? orders.error()} />
      </If>
      <If when={!positions.error() && positions()}>
        <Card title="POSITIONS">
          <If when={(positions() ?? []).length === 0}>
            <Line color={MUTED}>None.</Line>
          </If>
          <Each of={positions() ?? []}>
            {(position) => <Line>{`${position().symbol}  ${position().qty} @ ${position().avgCost}`}</Line>}
          </Each>
        </Card>
      </If>
      <If when={!orders.error() && orders()}>
        <Card title="OPEN ORDERS">
          <If when={(orders() ?? []).length === 0}>
            <Line color={MUTED}>None.</Line>
          </If>
          <Each of={orders() ?? []}>
            {(order) => (
              <Line>{`${order().side} ${order().qty} ${order().symbol} (${order().state})`}</Line>
            )}
          </Each>
        </Card>
      </If>
    </View>
  );
}

function GatesScreen(props: { url: () => string | null }) {
  const versions = resource(props.url, (url) => Nisse.versions(url));
  const tone = (state: string) => (state === "pass" ? GREEN : state === "fail" ? RED : MUTED);
  return (
    <View style={{ gap: 16 }}>
      <If when={!props.url()}>
        <NotConnected />
      </If>
      <If when={versions.error()}>
        <CannotConfirm what="the gate reports" error={versions.error()} />
      </If>
      <If when={!versions.error() && versions() && (versions() ?? []).length === 0}>
        <Card title="STRATEGY VERSIONS">
          <Line color={MUTED}>None yet.</Line>
        </Card>
      </If>
      <Each of={versions() ?? []}>
        {(version) => (
          <Card title={`VERSION ${version().id}  ${version().gitSha.slice(0, 7)}`}>
            <If when={version().gates.length === 0}>
              <Line color={MUTED}>No gate report stored.</Line>
            </If>
            <Each of={version().gates}>
              {(gate) => <Line color={tone(gate().state)}>{`${gate().name}: ${gate().state.replace("_", " ")}`}</Line>}
            </Each>
          </Card>
        )}
      </Each>
      <Line color={MUTED}>
        A strategy is promoted on these gates over the whole trial history, never on recent results.
      </Line>
    </View>
  );
}

function ProposalsScreen() {
  const pulls = resource(async ({ signal }) => {
    const response = await fetch(PULLS_URL, { signal, headers: { Accept: "application/vnd.github+json" } });
    if (!response.ok) throw new Error(`GitHub answered ${response.status}`);
    return (await response.json()) as Pull[];
  });
  const origin = (pull: Pull) =>
    pull.head.repo && pull.head.repo.full_name !== pull.base.repo.full_name ? "from a fork" : "from this repository";
  return (
    <View style={{ gap: 16 }}>
      <If when={pulls.error()}>
        <Card title="PULL REQUESTS">
          <Line color={RED}>{`Could not list pull requests: ${String(pulls.error())}`}</Line>
        </Card>
      </If>
      <If when={!pulls.error() && pulls() && (pulls() ?? []).length === 0}>
        <Card title="PULL REQUESTS">
          <Line color={MUTED}>None open.</Line>
        </Card>
      </If>
      <Each of={pulls() ?? []}>
        {(pull) => (
          <Card title={`#${pull().number}  ${pull().user?.login ?? "unknown"}, ${origin(pull())}`}>
            <Line>{pull().title}</Line>
            <Button title="Open on GitHub" onPress={() => void openUrl(pull().html_url)} />
          </Card>
        )}
      </Each>
      <Line color={MUTED}>Reviewing and merging happen on GitHub. Treat an agent's pull request as untrusted.</Line>
      <Button title="Refresh" onPress={() => pulls.refetch()} />
    </View>
  );
}

function KillScreen(props: { url: () => string | null }) {
  const name = signal("");
  const armed = signal(false);
  const busy = signal(false);
  // What the last attempt established: nothing yet, engaged (read back), or not confirmed.
  const result = signal<{ ok: boolean; text: string } | null>(null);

  const engage = async () => {
    const url = props.url();
    if (!url) return;
    busy.set(true);
    result.set(null);
    try {
      const row = await Nisse.engageKill(url, name());
      result.set({
        ok: true,
        text:
          `Read back from the store (row ${row}). No new orders will be sent. ` +
          "Open orders are cancelled by the engine's next run, within two minutes in market hours.",
      });
    } catch (error) {
      result.set({ ok: false, text: `NOT CONFIRMED: ${String(error)}` });
    } finally {
      busy.set(false);
      armed.set(false);
    }
  };

  return (
    <View style={{ gap: 16 }}>
      <If when={!props.url()}>
        <NotConnected />
      </If>
      <Card title="KILL SWITCH">
        <Line>Stops the engine from sending orders. Releasing it is done on the engine host, not here.</Line>
        <Input
          value={name()}
          onChange={({ value }) => {
            name.set(value);
            armed.set(false);
          }}
          placeholder="Your name"
          autoCapitalize="words"
          maxLength={64}
          style={{ fontSize: 16, padding: 12, borderWidth: 1, borderColor: "#d0d5dd", borderRadius: 8, backgroundColor: PAPER }}
        />
        <If when={!armed()}>
          <Button
            title="Engage kill switch"
            tone="danger"
            disabled={!props.url() || !name().trim() || busy()}
            onPress={() => armed.set(true)}
          />
        </If>
        <If when={armed()}>
          <Line color={RED} strong>{`Engage the kill switch as ${name().trim()}?`}</Line>
          <Button title="Yes, engage now" tone="danger" disabled={busy()} onPress={() => void engage()} />
          <Button title="Cancel" disabled={busy()} onPress={() => armed.set(false)} />
        </If>
      </Card>
      <If when={result()}>
        <Card title={result()?.ok ? "RESULT" : "NOT CONFIRMED"}>
          <If when={result()?.ok}>
            <Line color={RED} strong>Kill switch ENGAGED</Line>
          </If>
          <Line color={result()?.ok ? INK : RED} strong={!result()?.ok}>{result()?.text ?? ""}</Line>
          <If when={!result()?.ok}>
            <Line color={MUTED}>{OUT_OF_BAND}</Line>
          </If>
        </Card>
      </If>
    </View>
  );
}

function SettingsScreen(props: { url: () => string | null; save: (url: string | null) => void }) {
  const draft = signal("");
  const saved = signal(false);
  return (
    <View style={{ gap: 16 }}>
      <Card title="STORE CONNECTION">
        <Line>{props.url() ? "A connection is saved." : "No connection is saved."}</Line>
        <Input
          value={draft()}
          onChange={({ value }) => {
            draft.set(value);
            saved.set(false);
          }}
          placeholder="postgres://user:password@host/db"
          masked
          autoCorrect={false}
          autoCapitalize="none"
          style={{ fontSize: 16, padding: 12, borderWidth: 1, borderColor: "#d0d5dd", borderRadius: 8, backgroundColor: PAPER }}
        />
        <Button
          title="Save"
          tone="primary"
          disabled={!draft().trim()}
          onPress={() => {
            props.save(draft().trim());
            draft.set("");
            saved.set(true);
          }}
        />
        <If when={saved()}>
          <Line color={GREEN}>Saved.</Line>
        </If>
        <Button title="Forget the connection" disabled={!props.url()} onPress={() => props.save(null)} />
      </Card>
      <Card title="READ THIS">
        <Line color={MUTED}>
          Use the login made for this phone (a member of nisse_ui): it can read the store and engage the kill switch,
          and nothing else.
        </Line>
        <Line color={MUTED}>
          The connection is not encrypted by the app. Reach the database over a private network (a VPN or an SSH
          tunnel), never over the open internet.
        </Line>
        <Line color={MUTED}>
          The connection string is kept in this app's storage on the device, which is not an encrypted keychain.
        </Line>
      </Card>
    </View>
  );
}

function App() {
  const tab = signal<Tab>("status");
  // `undefined` until storage has been read; `null` when nothing is saved.
  const url = signal<string | null | undefined>(undefined);
  void Storage.getItem(URL_KEY).then(
    (saved) => url.set(saved),
    () => url.set(null),
  );
  const connection = () => url() ?? null;
  const save = (next: string | null) => {
    url.set(next);
    void (next === null ? Storage.removeItem(URL_KEY) : Storage.setItem(URL_KEY, next));
  };

  return (
    <View
      style={{
        flex: 1,
        backgroundColor: PAPER,
        paddingTop: "env(safe-area-inset-top)",
        paddingRight: "env(safe-area-inset-right)",
        paddingBottom: "env(safe-area-inset-bottom)",
        paddingLeft: "env(safe-area-inset-left)",
      }}
    >
      <View style={{ paddingHorizontal: 16, paddingVertical: 12 }}>
        <Text style={{ fontSize: 22, fontWeight: "bold", color: INK }}>Nisse</Text>
      </View>
      <View style={{ flexDirection: "row", flexWrap: "wrap", gap: 8, paddingHorizontal: 16, paddingBottom: 12 }}>
        <Each of={TABS}>
          {(item) => (
            <Button
              title={item().title}
              tone={item().id === "kill" ? "danger" : tab() === item().id ? "primary" : "plain"}
              onPress={() => tab.set(item().id)}
            />
          )}
        </Each>
      </View>
      <Scroll style={{ flex: 1 }} contentStyle={{ padding: 16, gap: 16 }}>
        <If when={tab() === "status"}>
          <StatusScreen url={connection} />
        </If>
        <If when={tab() === "positions"}>
          <PositionsScreen url={connection} />
        </If>
        <If when={tab() === "gates"}>
          <GatesScreen url={connection} />
        </If>
        <If when={tab() === "proposals"}>
          <ProposalsScreen />
        </If>
        <If when={tab() === "kill"}>
          <KillScreen url={connection} />
        </If>
        <If when={tab() === "settings"}>
          <SettingsScreen url={connection} save={save} />
        </If>
      </Scroll>
    </View>
  );
}

mount(() => <Window id="main">{App}</Window>);
