import { createSignal, onMount, Show, For } from "solid-js";
import { Button, Field, Input, Select, Segmented, Toggle, Badge, toast } from "../ui/kit.jsx";
import { rpc } from "../lib/rpc.js";

export function RemoteExecutionSection(props) {
  const [cfg, setCfg] = createSignal(null);
  const [contexts, setContexts] = createSignal([]);
  const [currentCtx, setCurrentCtx] = createSignal("");
  const [kubectlOk, setKubectlOk] = createSignal(true);
  const [eksClusters, setEksClusters] = createSignal([]);
  const [eksBusy, setEksBusy] = createSignal(false);
  const [testing, setTesting] = createSignal(false);
  const [saving, setSaving] = createSignal(false);
  const [testResult, setTestResult] = createSignal(null);
  const [pods, setPods] = createSignal([]);

  async function loadAll() {
    const [c, ctxs] = await Promise.all([
      rpc("k8s_exec.get", {}, { local: true }).catch(() => null),
      rpc("k8s_exec.contexts", {}, { local: true }).catch(() => ({ available: false, contexts: [] })),
    ]);
    if (c) setCfg(c);
    setKubectlOk(ctxs?.available !== false);
    setContexts(ctxs?.contexts ?? []);
    setCurrentCtx(ctxs?.current ?? "");
    if (c?.enabled) {
      rpc("k8s_exec.pods", {}, { local: true })
        .then((r) => setPods(r?.pods ?? []))
        .catch(() => setPods([]));
    }
  }

  onMount(loadAll);

  const patch = (key, val) => setCfg((prev) => ({ ...(prev ?? {}), [key]: val }));

  async function save(next = cfg()) {
    if (!next || saving()) return;
    setSaving(true);
    try {
      const saved = await rpc("k8s_exec.set", next, { local: true });
      setCfg(saved);
      toast("Remote execution settings saved", "secure");
    } catch (e) {
      toast(String(e), "danger");
    } finally {
      setSaving(false);
    }
  }

  async function testConnection() {
    if (testing()) return;
    setTesting(true);
    setTestResult(null);
    try {
      await rpc("k8s_exec.set", cfg(), { local: true });
      const r = await rpc("k8s_exec.test", {}, { local: true });
      setTestResult(r);
      if (r?.ok) {
        toast(`Connected to ${r.server_version || "cluster"}`, "secure");
        const p = await rpc("k8s_exec.pods", {}, { local: true }).catch(() => null);
        if (p?.pods) setPods(p.pods);
      } else {
        toast(r?.error || "Cluster connection failed", "danger");
      }
    } catch (e) {
      setTestResult({ ok: false, error: String(e) });
      toast(String(e), "danger");
    } finally {
      setTesting(false);
    }
  }

  async function discoverEks() {
    if (eksBusy()) return;
    setEksBusy(true);
    try {
      const r = await rpc("k8s_exec.eks_clusters", { region: cfg()?.eks_region || "" }, { local: true });
      if (r?.error) toast(r.error, "danger");
      setEksClusters(Array.isArray(r?.clusters) ? r.clusters : []);
      if (r?.clusters?.length && !cfg()?.eks_cluster) patch("eks_cluster", r.clusters[0]);
    } catch (e) {
      toast(String(e), "danger");
    } finally {
      setEksBusy(false);
    }
  }

  async function connectEks() {
    const region = cfg()?.eks_region?.trim();
    const cluster = cfg()?.eks_cluster?.trim();
    if (!region || !cluster) return toast("Enter AWS region and EKS cluster name", "warn");
    setEksBusy(true);
    try {
      const r = await rpc("k8s_exec.eks_connect", { region, cluster }, { local: true });
      toast(`Connected kubeconfig to ${r.context || cluster}`, "secure");
      await loadAll();
    } catch (e) {
      toast(String(e), "danger");
    } finally {
      setEksBusy(false);
    }
  }

  async function deletePod(name) {
    try {
      await rpc("k8s_exec.delete_pod", { pod: name, namespace: cfg()?.namespace }, { local: true });
      toast(`Deleted pod ${name}`);
      setPods((list) => list.filter((p) => p.name !== name));
    } catch (e) {
      toast(String(e), "danger");
    }
  }

  return (
    <section class="ws-section">
      <div class="ws-row">
        <div>
          <h2>Remote execution (k3s / EKS)</h2>
          <p class="ws-muted small">
            Run isolated agent pods and test workloads on your home k3s or AWS EKS cluster instead of local Docker/kind.
          </p>
        </div>
        <div class="ws-actions">
          <Badge tone={cfg()?.enabled ? "secure" : ""}>{cfg()?.enabled ? "Enabled" : "Local only"}</Badge>
          <Show when={cfg()}>
            <Toggle
              on={!!cfg().enabled}
              disabled={saving() || props.busy}
              label=""
              onChange={(v) => { patch("enabled", v); save({ ...cfg(), enabled: v }); }}
            />
          </Show>
        </div>
      </div>

      <Show when={!kubectlOk()}>
        <p class="ws-alert">kubectl was not found on PATH. Install kubectl (e.g. brew install kubernetes-cli) to use remote cluster execution.</p>
      </Show>

      <Show when={cfg()}>
        <Field label="Cluster type" hint="Home k3s cluster, AWS EKS, or any standard Kubernetes context.">
          <Segmented
            value={cfg().provider || "k3s"}
            options={[
              { value: "k3s", label: "k3s / Home" },
              { value: "eks", label: "AWS EKS" },
              { value: "generic", label: "Generic K8s" },
            ]}
            onChange={(v) => patch("provider", v)}
          />
        </Field>

        <Show when={cfg().provider === "eks"}>
          <div class="ws-row">
            <Field label="AWS region" hint="e.g. eu-north-1 or us-east-1">
              <Input mono value={cfg().eks_region || ""} onValue={(v) => patch("eks_region", v)} placeholder="eu-north-1" />
            </Field>
            <Field label="EKS cluster" hint="Discover via your logged-in AWS CLI or type the cluster name.">
              <div class="row" style={{ gap: "6px" }}>
                <Show
                  when={eksClusters().length > 0}
                  fallback={<Input mono value={cfg().eks_cluster || ""} onValue={(v) => patch("eks_cluster", v)} placeholder="cluster-name" />}
                >
                  <Select
                    value={cfg().eks_cluster || ""}
                    options={eksClusters().map((c) => ({ value: c, label: c }))}
                    onValue={(v) => patch("eks_cluster", v)}
                  />
                </Show>
                <Button size="sm" variant="ghost" busy={eksBusy()} onClick={discoverEks}>List</Button>
                <Button size="sm" busy={eksBusy()} onClick={connectEks}>Update kubeconfig</Button>
              </div>
            </Field>
          </div>
        </Show>

        <div class="ws-row">
          <Field label="Kubeconfig context" hint={currentCtx() ? `Default context: ${currentCtx()}` : "Leave empty to use current-context."}>
            <Show
              when={contexts().length > 0}
              fallback={<Input mono value={cfg().context || ""} onValue={(v) => patch("context", v)} placeholder={currentCtx() || "default"} />}
            >
              <Select
                value={cfg().context || ""}
                options={[{ value: "", label: currentCtx() ? `Current (${currentCtx()})` : "Default context" }, ...contexts().map((c) => ({ value: c, label: c }))]}
                onValue={(v) => patch("context", v)}
              />
            </Show>
          </Field>
          <Field label="Namespace" hint="Dedicated namespace for Philium agent pods.">
            <Input mono value={cfg().namespace || "philium-agents"} onValue={(v) => patch("namespace", v)} placeholder="philium-agents" />
          </Field>
        </div>

        <div class="ws-row">
          <Field label="Runner container image" hint="Registry image used for remote agent pods (required to create pods).">
            <Input mono value={cfg().image || ""} onValue={(v) => patch("image", v)} placeholder="ghcr.io/your-org/agent-runner" />
          </Field>
          <Field label="Kubeconfig path (optional)" hint="Leave blank to use ~/.kube/config.">
            <Input mono value={cfg().kubeconfig || ""} onValue={(v) => patch("kubeconfig", v)} placeholder="~/.kube/config" />
          </Field>
        </div>

        <div class="ws-row">
          <Field label="CPU request" hint="Per-pod CPU request (e.g. 500m, 2).">
            <Input mono value={cfg().cpu_request || "500m"} onValue={(v) => patch("cpu_request", v)} placeholder="500m" />
          </Field>
          <Field label="Memory request" hint="Per-pod memory request (e.g. 1Gi, 4Gi).">
            <Input mono value={cfg().memory_request || "1Gi"} onValue={(v) => patch("memory_request", v)} placeholder="1Gi" />
          </Field>
        </div>

        <div class="ws-row">
          <div class="row" style={{ gap: "8px", "flex-wrap": "wrap" }}>
            <Show when={testResult()}>
              {(r) => (
                <Badge tone={r().ok ? "secure" : "danger"}>
                  {r().ok
                    ? `${r().server_version || "Connected"} · ${r().can_create_pods ? "can create pods" : "check RBAC"} · ns ${r().namespace_exists ? "ready" : "will be created"}`
                    : r().error || "Failed"}
                </Badge>
              )}
            </Show>
          </div>
          <div class="ws-actions">
            <Button size="sm" variant="ghost" busy={testing()} onClick={testConnection}>Test connection</Button>
            <Button size="sm" variant="primary" busy={saving()} onClick={() => save()}>Save</Button>
          </div>
        </div>

        <Show when={pods().length > 0}>
          <div class="col" style={{ gap: "6px", "margin-top": "8px" }}>
            <span class="xs dim">Managed pods in {cfg().namespace} ({pods().length})</span>
            <For each={pods()}>
              {(p) => (
                <div class="ws-row" style={{ padding: "6px 10px", background: "var(--bg-sunken)", "border-radius": "6px" }}>
                  <div class="row" style={{ gap: "8px" }}>
                    <code class="mono xs">{p.name}</code>
                    <Badge tone={p.phase === "Running" ? "secure" : ""}>{p.phase}</Badge>
                    <Show when={p.node}><span class="xs faint">{p.node}</span></Show>
                  </div>
                  <Button size="sm" variant="ghost" onClick={() => deletePod(p.name)}>Delete</Button>
                </div>
              )}
            </For>
          </div>
        </Show>
      </Show>
    </section>
  );
}
