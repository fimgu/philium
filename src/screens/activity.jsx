import {
  For,
  Show,
  createSignal,
  createMemo,
  createEffect,
  onMount,
  onCleanup,
} from "solid-js";
import { Page, Button, Empty, VirtualList, Shield, confirm } from "../ui/kit.jsx";
import { rpc, useEvent, fmt } from "../lib/rpc.js";
import { useScreenVisible } from "../lib/visibility.js";
import "../styles/terminals.css";
export default function Activity() {
  const visible = useScreenVisible();
  const [rows, setRows] = createSignal([]),
    [provider, setProvider] = createSignal(""),
    [agent, setAgent] = createSignal(""),
    [guard, setGuard] = createSignal(""),
    [detail, setDetail] = createSignal(null),
    [selected, setSelected] = createSignal(null),
    [error, setError] = createSignal("");
  let timer = null,
    pending = new Map(),
    detailGeneration = 0;
  const flush = () => {
    timer = null;
    if (!visible() || pending.size === 0) return;
    const updates = pending;
    pending = new Map();
    setRows((old) => {
      const all = new Map(old.map((r) => [r.id, r]));
      for (const [id, r] of updates) all.set(id, r);
      return [...all.values()].sort((a, b) => b.id - a.id).slice(0, 500);
    });
    if (visible() && selected() && updates.has(selected())) inspect(selected());
  };
  useEvent("activity", (a) => {
    if (!a?.id) return;
    if (pending.has(a.id)) pending.delete(a.id);
    pending.set(a.id, a);
    while (pending.size > 500) {
      const oldest = pending.keys().next().value;
      pending.delete(oldest);
    }
    if (!visible()) return;
    if (!timer) timer = setTimeout(flush, 100);
  });
  createEffect(() => {
    if (visible() && pending.size > 0) {
      clearTimeout(timer);
      timer = null;
      flush();
    }
  });
  onMount(async () => {
    try {
      const list = await rpc("activity.list");
      setRows((old) => {
        const all = new Map(list.map((r) => [r.id, r]));
        for (const r of old) all.set(r.id, r);
        return [...all.values()].sort((a, b) => b.id - a.id).slice(0, 500);
      });
    } catch (e) {
      setError(String(e));
    }
  });
  onCleanup(() => {
    clearTimeout(timer);
    detailGeneration++;
  });
  const filtered = createMemo(() =>
    rows().filter(
      (r) =>
        (!provider() || r.provider === provider()) &&
        (!agent() || (r.agent || "desk") === agent()) &&
        (!guard() || r.guard === guard()),
    ),
  );
  const inspect = async (id) => {
    const g = ++detailGeneration;
    setSelected(id);
    try {
      const d = await rpc("activity.detail", { id });
      if (g === detailGeneration) {
        setDetail(d);
        if (!d) setError("This request has expired from the live log.");
        else setError("");
      }
    } catch (e) {
      if (g === detailGeneration) setError(String(e));
    }
  };
  const clear = async () => {
    if (
      !(await confirm(
        "Clear live gateway log?",
        "Removes all recent requests from the in-memory live activity log. Historical token statistics in Stats are kept.",
        { danger: true, ok: "Clear live log" },
      ))
    )
      return;
    try {
      await rpc("activity.clear");
      clearTimeout(timer);
      timer = null;
      pending.clear();
      setRows([]);
      setDetail(null);
      setSelected(null);
      setError("");
      detailGeneration++;
    } catch (e) {
      setError(String(e));
    }
  };
  return (
    <Page
      title="Activity"
      sub="What left my machine: live, guard-scanned gateway traffic"
      flush
      actions={
        <>
          <Button
            onClick={() => {
              setProvider("");
              setAgent("");
              setGuard("");
            }}
          >
            Reset filters
          </Button>
          <Button onClick={clear}>Clear live log</Button>
        </>
      }
    >
      <div class="activity-screen">
        <Shield tone="good">
          Request details show the exact body forwarded after guard processing.
          Blocked requests never leave this machine.
        </Shield>
        <Show when={error()}>
          <p role="alert">{error()}</p>
        </Show>
        <div class="activity-filters">
          <label>
            Provider{" "}
            <select
              value={provider()}
              onChange={(e) => setProvider(e.currentTarget.value)}
            >
              <option value="">All providers</option>
              <For each={[...new Set(rows().map((r) => r.provider))]}>
                {(p) => <option value={p}>{p}</option>}
              </For>
            </select>
          </label>
          <label>
            Agent{" "}
            <select
              value={agent()}
              onChange={(e) => setAgent(e.currentTarget.value)}
            >
              <option value="">All agents</option>
              <For each={[...new Set(rows().map((r) => r.agent || "desk"))]}>
                {(a) => <option value={a}>{a}</option>}
              </For>
            </select>
          </label>
          <label>
            Guard{" "}
            <select
              value={guard()}
              onChange={(e) => setGuard(e.currentTarget.value)}
            >
              <option value="">All guard states</option>
              <For each={["pass", "redacted", "blocked", "flagged"]}>
                {(g) => <option value={g}>{g}</option>}
              </For>
            </select>
          </label>
          <span>{filtered().length} requests</span>
        </div>
        <div class="activity-layout">
          <div class="activity-list">
            <Show
              when={filtered().length}
              fallback={
                <Empty
                  title={
                    rows().length
                      ? "No matching requests"
                      : "See what your agents send"
                  }
                  action={
                    <Button
                      onClick={() => {
                        setProvider("");
                        setAgent("");
                        setGuard("");
                      }}
                    >
                      Show all traffic
                    </Button>
                  }
                >
                  Requests appear live when desk chat or an agent calls a model.
                  Select a row to inspect what left your machine.
                </Empty>
              }
            >
              <VirtualList items={filtered()} rowHeight={72}>
                {(r) => (
                  <button
                    class={`activity-row guard-${r.guard} ${selected() === r.id ? "selected" : ""}`}
                    onClick={() => inspect(r.id)}
                  >
                    <div>
                      <strong>
                        {r.provider} / {r.model || "pending model"}
                      </strong>
                      <span>
                        {r.agent || "desk"} · {r.state} ·{" "}
                        {r.status || "pending"}
                      </span>
                    </div>
                    <div>
                      <span>
                        {r.guard === "blocked"
                          ? "BLOCKED · nothing sent"
                          : r.guard === "redacted"
                            ? "REDACTED · sanitized before sending"
                            : r.guard === "flagged"
                              ? "WARNING · guard disabled"
                              : "PASS · forwarded"}
                      </span>
                      <span>
                        TTFT {fmt.ms(r.ttft_ms)} · {fmt.compact(r.input_tokens)}{" "}
                        in / {fmt.compact(r.output_tokens)} out ·{" "}
                        {fmt.ago(r.started_ms)}
                      </span>
                    </div>
                  </button>
                )}
              </VirtualList>
            </Show>
          </div>
          <Show when={detail()}>
            <aside class="activity-drawer" aria-label="Request detail">
              <header>
                <h2>What left my machine</h2>
                <Button
                  onClick={() => {
                    detailGeneration++;
                    setDetail(null);
                    setSelected(null);
                  }}
                >
                  Close
                </Button>
              </header>
              <Shield
                tone={
                  detail().summary.guard === "blocked"
                    ? "danger"
                    : detail().summary.guard === "pass"
                      ? "good"
                      : "strict"
                }
              >
                {detail().summary.guard === "blocked"
                  ? "Blocked locally. This request did not leave your machine."
                  : `Forwarded request body · guard: ${detail().summary.guard}`}
              </Shield>
              <p>
                {detail().summary.method} {detail().summary.path}
              </p>
              <p>
                Agent: {detail().summary.agent || "desk"} · TTFT:{" "}
                {fmt.ms(detail().summary.ttft_ms)}
              </p>
              <h3>Guard findings</h3>
              <pre>{JSON.stringify(detail().summary.findings, null, 2)}</pre>
              <Show
                when={detail().summary.guard !== "blocked"}
                fallback={
                  <p>No outgoing body. The gateway refused this request.</p>
                }
              >
                <h3>Exact outgoing request body</h3>
                <pre>{detail().request}</pre>
              </Show>
              <Show when={detail().response_head || detail().response_tail}>
                <h3>Response preview</h3>
                <Show when={detail().response_head}>
                  <div class="xs faint" style={{ "margin-bottom": "4px" }}>
                    Response head
                  </div>
                  <pre>{detail().response_head}</pre>
                </Show>
                <Show
                  when={
                    detail().response_tail &&
                    detail().response_tail !== detail().response_head
                  }
                >
                  <div class="xs faint" style={{ margin: "8px 0 4px" }}>
                    Response tail
                  </div>
                  <pre>{detail().response_tail}</pre>
                </Show>
              </Show>
              <Show when={detail().summary.error}>
                <p role="alert">{detail().summary.error}</p>
              </Show>
            </aside>
          </Show>
        </div>
      </div>
    </Page>
  );
}
