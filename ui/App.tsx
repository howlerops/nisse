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
/** A read older than this is no longer shown as the state: two missed refreshes. */
const STALE_MS = 75_000;
/** A heartbeat older than this (seconds) is shown as late. The engine ticks every two minutes. */
const HEARTBEAT_LATE_SECONDS = 300;
/** The kill watcher runs every minute; one that has not run for this long (seconds) is not relied on. */
const WATCHER_LATE_SECONDS = 180;
/** The repository whose open pull requests the Proposals screen lists. */
const PULLS_URL = "https://api.github.com/repos/howlerops/nisse/pulls?state=open&per_page=30";

const RED = "#b42318";
const GREEN = "#067647";
const AMBER = "#b54708";
const INK = "#101828";
const MUTED = "#475467";
const PAPER = "#ffffff";
const WASH = "#f2f4f7";

type Tab = "status" | "positions" | "gates" | "activity" | "proposals" | "kill" | "settings";
const TABS: { id: Tab; title: string }[] = [
  { id: "status", title: "Status" },
  { id: "positions", title: "Positions" },
  { id: "gates", title: "Gates" },
  { id: "activity", title: "Activity" },
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

/**
 * A read of the store that is shown only while it is recent. It is re-read every `REFRESH_MS`
 * while the calling screen is on show; `value()` is the last read only if that read succeeded
 * and is younger than `STALE_MS` by this device's clock, so a screen left open, or an app
 * brought back from the background, never goes on showing an old answer. `problem()` is why
 * there is no value, when there is a reason.
 */
function freshRead<T>(url: () => string | null, read: (url: string) => Promise<T>) {
  const readAt = signal<number | null>(null);
  const clock = signal(Date.now());
  const data = resource(url, async (connection, info) => {
    const value = await read(connection);
    // A load that was overtaken by a newer one must not count as a fresh read.
    if (!info.signal.aborted) readAt.set(Date.now());
    return value;
  });
  const tick = setInterval(() => clock.set(Date.now()), 1000);
  const refresh = setInterval(() => data.refetch(), REFRESH_MS);
  cleanup(() => {
    clearInterval(tick);
    clearInterval(refresh);
  });
  // The distance from the read, whichever way the device's clock has moved since.
  const age = () => (readAt() === null ? null : Math.abs(clock() - (readAt() ?? 0)));
  const ageSeconds = () => Math.round((age() ?? 0) / 1000);
  const stale = () => (age() ?? 0) > STALE_MS;
  return {
    value: (): T | undefined => (!data.error() && !stale() && readAt() !== null ? data() : undefined),
    problem: (): unknown =>
      data.error() ?? (stale() ? `The last read is ${ageSeconds()} s old and has not been refreshed.` : undefined),
    ageSeconds,
    refetch: () => data.refetch(),
  };
}

/** "just now", "5 min ago", "3 h ago", "2 d ago". */
function ago(seconds: number): string {
  if (seconds < 60) return "just now";
  if (seconds < 90 * 60) return `${Math.round(seconds / 60)} min ago`;
  if (seconds < 48 * 3600) return `${Math.round(seconds / 3600)} h ago`;
  return `${Math.round(seconds / 86400)} d ago`;
}

/** Whether something that should run every minute or two ran recently enough. */
function recent(seconds: number | null | undefined, limit: number): boolean {
  return seconds !== null && seconds !== undefined && seconds <= limit;
}

function StatusScreen(props: { url: () => string | null }) {
  const status = freshRead(props.url, (url) => Nisse.status(url));
  const alive = () => recent(status.value()?.heartbeatAgeSeconds, HEARTBEAT_LATE_SECONDS);
  const watching = () => recent(status.value()?.watcherAgeSeconds, WATCHER_LATE_SECONDS);
  const heartbeat = () => {
    const seconds = status.value()?.heartbeatAgeSeconds;
    if (seconds === null || seconds === undefined) return "Heartbeat: never";
    return `Heartbeat: ${Math.round(seconds)} s before this read${alive() ? "" : " (late)"}`;
  };
  const watcher = () => {
    const seconds = status.value()?.watcherAgeSeconds;
    if (seconds === null || seconds === undefined) return "Kill watcher: never ran";
    return `Kill watcher: ran ${Math.round(seconds)} s before this read${watching() ? "" : " (late)"}`;
  };
  // "off" is only good news when the engine is known to be alive.
  const switchLine = () => {
    if (status.value()?.killSwitch) return { text: "Kill switch: ENGAGED", color: RED };
    if (alive()) return { text: "Kill switch: off", color: GREEN };
    return { text: "Kill switch: off (engine not confirmed alive)", color: AMBER };
  };
  return (
    <View style={{ gap: 16 }}>
      <If when={!props.url()}>
        <NotConnected />
      </If>
      <If when={status.problem()}>
        <CannotConfirm what="the engine's state" error={status.problem()} />
      </If>
      <If when={status.value()}>
        <Card title="ENGINE">
          <Line color={switchLine().color} strong>{switchLine().text}</Line>
          <Line>{`Drawdown mode: ${status.value()?.drawdownMode ?? ""}`}</Line>
          <Line>{`Equity peak: ${status.value()?.equityPeak ?? "none yet"}`}</Line>
          <Line color={alive() ? INK : AMBER}>{heartbeat()}</Line>
          <Line color={watching() ? INK : AMBER}>{watcher()}</Line>
          <Line color={status.value()?.rebalanceOwed ? AMBER : INK}>
            {status.value()?.rebalanceOwed ? "Rebalance owed: yes" : "Rebalance owed: no"}
          </Line>
          <Line color={MUTED}>{`Limits: ${status.value()?.limitsHash ?? "none recorded"}`}</Line>
          <Line color={MUTED}>{`Read ${status.ageSeconds()} s ago`}</Line>
        </Card>
      </If>
      <Button title="Refresh" onPress={status.refetch} />
    </View>
  );
}

function PositionsScreen(props: { url: () => string | null }) {
  const positions = freshRead(props.url, (url) => Nisse.positions(url));
  const orders = freshRead(props.url, (url) => Nisse.orders(url));
  return (
    <View style={{ gap: 16 }}>
      <If when={!props.url()}>
        <NotConnected />
      </If>
      <If when={positions.problem() ?? orders.problem()}>
        <CannotConfirm what="positions and orders" error={positions.problem() ?? orders.problem()} />
      </If>
      <If when={positions.value()}>
        <Card title="POSITIONS">
          <If when={(positions.value() ?? []).length === 0}>
            <Line color={MUTED}>None.</Line>
          </If>
          <Each of={positions.value() ?? []}>
            {(position) => <Line>{`${position().symbol}  ${position().qty} @ ${position().avgCost}`}</Line>}
          </Each>
          <Line color={MUTED}>{`Read ${positions.ageSeconds()} s ago`}</Line>
        </Card>
      </If>
      <If when={orders.value()}>
        <Card title="OPEN ORDERS, AS THE ENGINE LAST RECORDED THEM">
          <If when={(orders.value() ?? []).length === 0}>
            <Line color={MUTED}>None.</Line>
          </If>
          <Each of={orders.value() ?? []}>
            {(order) => (
              <Line>{`${order().side} ${order().qty} ${order().symbol} (${order().state})`}</Line>
            )}
          </Each>
          <Line color={MUTED}>
            This list changes when an engine run reconciles with the broker, not at the moment an order is
            cancelled. The broker's own dashboard is the live view.
          </Line>
        </Card>
      </If>
      <Button
        title="Refresh"
        onPress={() => {
          positions.refetch();
          orders.refetch();
        }}
      />
    </View>
  );
}

function GatesScreen(props: { url: () => string | null }) {
  const versions = freshRead(props.url, (url) => Nisse.versions(url));
  const tone = (state: string) => (state === "pass" ? GREEN : state === "fail" ? RED : MUTED);
  return (
    <View style={{ gap: 16 }}>
      <If when={!props.url()}>
        <NotConnected />
      </If>
      <If when={versions.problem()}>
        <CannotConfirm what="the gate reports" error={versions.problem()} />
      </If>
      <If when={versions.value() && (versions.value() ?? []).length === 0}>
        <Card title="STRATEGY VERSIONS">
          <Line color={MUTED}>None yet.</Line>
        </Card>
      </If>
      <Each of={versions.value() ?? []}>
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
      <Button title="Refresh" onPress={versions.refetch} />
    </View>
  );
}

function ActivityScreen(props: { url: () => string | null }) {
  const steps = freshRead(props.url, (url) => Nisse.steps(url));
  const tone = (status: string) => (status === "failed" ? RED : status === "started" ? AMBER : INK);
  return (
    <View style={{ gap: 16 }}>
      <If when={!props.url()}>
        <NotConnected />
      </If>
      <If when={steps.problem()}>
        <CannotConfirm what="what the engine last did" error={steps.problem()} />
      </If>
      <If when={steps.value()}>
        <Card title="ENGINE STEPS, NEWEST FIRST">
          <If when={(steps.value() ?? []).length === 0}>
            <Line color={MUTED}>None yet.</Line>
          </If>
          <Each of={steps.value() ?? []}>
            {(step) => (
              <Line color={tone(step().status)}>
                {`${step().runKey}  ${step().step}: ${step().status}, ${ago(step().ageSeconds)}`}
              </Line>
            )}
          </Each>
        </Card>
      </If>
      <Line color={MUTED}>
        A cancel made because of the kill switch is a "kill  cancel" row. Read its age: an old row is an old cancel.
      </Line>
      <Button title="Refresh" onPress={steps.refetch} />
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
  // What the last attempt established, and when. It is taken down after `STALE_MS`: the
  // switch can be released on the host, and this card must not go on saying "ENGAGED".
  const result = signal<{ ok: boolean; cancelling: boolean; text: string; at: number } | null>(null);
  const clock = signal(Date.now());
  const tick = setInterval(() => clock.set(Date.now()), 1000);
  cleanup(() => clearInterval(tick));
  const current = () => {
    const last = result();
    return last && Math.abs(clock() - last.at) <= STALE_MS ? last : null;
  };
  const expired = () => result() !== null && current() === null;

  const engage = async () => {
    const url = props.url();
    if (!url) return;
    busy.set(true);
    result.set(null);
    try {
      const row = await Nisse.engageKill(url, name());
      // The switch is engaged and read back. Whether open orders are being cancelled depends
      // on the watcher on the host, so say only what a fresh read shows about it.
      let watcherAge: number | null | undefined;
      try {
        watcherAge = (await Nisse.status(url)).watcherAgeSeconds;
      } catch {
        watcherAge = undefined;
      }
      const engaged = `Read back from the store (row ${row}). The engine sends no new orders from its next check.`;
      if (recent(watcherAge, WATCHER_LATE_SECONDS)) {
        result.set({
          ok: true,
          cancelling: true,
          at: Date.now(),
          text:
            `${engaged} The kill watcher on the host ran ${Math.round(watcherAge ?? 0)} s ago and cancels open ` +
            "orders within about a minute. Confirm it in Activity: a new \"kill  cancel\" row.",
        });
      } else {
        result.set({
          ok: true,
          cancelling: false,
          at: Date.now(),
          text:
            `${engaged} But the kill watcher on the host has not run recently, so open orders are NOT being ` +
            "cancelled. Cancel them at the broker now.",
        });
      }
    } catch (error) {
      result.set({ ok: false, cancelling: false, at: Date.now(), text: `NOT CONFIRMED: ${String(error)}` });
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
      <If when={current()}>
        <Card title={current()?.ok ? "RESULT" : "NOT CONFIRMED"}>
          <If when={current()?.ok}>
            <Line color={RED} strong>Kill switch ENGAGED</Line>
          </If>
          <If when={current()?.ok && !current()?.cancelling}>
            <Line color={RED} strong>Open orders are NOT being cancelled</Line>
          </If>
          <Line color={current()?.ok && current()?.cancelling ? INK : RED} strong={!current()?.ok}>
            {current()?.text ?? ""}
          </Line>
          <If when={!current()?.ok || !current()?.cancelling}>
            <Line color={MUTED}>{OUT_OF_BAND}</Line>
          </If>
        </Card>
      </If>
      <If when={expired()}>
        <Card title="RESULT">
          <Line color={MUTED}>The earlier result is no longer current. Read Status for the state now.</Line>
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
        <If when={tab() === "activity"}>
          <ActivityScreen url={connection} />
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
