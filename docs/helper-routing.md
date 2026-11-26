# Helper model routing (contract)

When an agent spawns a helper, Philium can pick the helper's model from several providers based on the job and on live usage. Off by default: with routing off, helpers keep the parent's model exactly as before.

## Privacy rule

Sending code to another company is the person's choice, per workspace.

- Routing only ever picks from providers the person ticked as **allowed** for that workspace. The parent's own provider is always allowed. An empty list means "parent's provider only".
- Only **trusted, enabled** providers can be picked. Untrusted providers are never routing targets.
- **Strict agents are never routed.** Their helpers stay on the parent's provider, Strict, exactly as today.
- A workspace override fully replaces the global config for that workspace. There is no merge: what the screen shows for a workspace is what applies.

## Config

Stored in kv as JSON. `routing` is the global default; `routing:<repo_id>` is an optional workspace override. Missing or unparsable config = routing off (fail closed, never wider).

```json
{
  "enabled": false,
  "allowed": ["<provider_id>", "..."],
  "jobs": {
    "explore": null,
    "code": null,
    "verify": null
  },
  "avoid_above_pct": 85,
  "verify_elsewhere": true
}
```

- `jobs.<job>`: a `provider/model` ref, or `null` = same as the parent. A ref whose provider is not allowed, not trusted or not enabled is ignored (reason says so).
- `avoid_above_pct`: skip a provider whose busiest usage window is at or above this percent, or which reports `limit_reached`.
- `verify_elsewhere`: for `verify` helpers, prefer an allowed provider different from the parent's, for a real second opinion.

## Jobs

`agent_spawn` gains an optional `job`: `explore` (search, read, run tests), `code` (write code, default), `verify` (confirm another agent's finding). If omitted: role `reviewer`/`verifier` -> `verify`, otherwise `code`. Orchestrator briefs (agents.rs, pr_review.rs, review_run.rs) mention `job`.

## Choosing (pure function, unit tested)

Inputs: config, parent model ref, job, explicit `model` arg (optional), provider list, usage per provider (max window percent, `limit_reached`).

1. Routing off, or parent Strict: model = parent's (explicit `model` must equal it, as today). Reason `same as parent`.
2. Explicit `model` from the agent: allowed only if its provider is allowed+trusted+enabled; otherwise error `"<provider> is not allowed for helpers in this workspace"`. Not usage-filtered (the agent asked for it).
3. Candidates in order: `jobs[job]` if set; for `verify` with `verify_elsewhere`, each other allowed provider's default model; the parent's model; then each other allowed provider's default model (allowed-list order). Deduplicate.
4. Drop candidates that are over `avoid_above_pct` or `limit_reached`. Pick the first left.
5. If none left: pick the candidate with the lowest usage. Reason says all were busy.
6. Usage unknown for a provider counts as available.

Output: `{model_ref, reason}`. `reason` is a short plain sentence, e.g. `Explore job: fast model`, `Anthropic at 92%, used Google`, `Second opinion on another provider`, `Same as parent`.

Usage source: the last `account_usage` snapshot (no network on spawn) merged with `gateway.quotas()`; per provider take the highest window percent.

## Enforcement

- `helper_scope` (agents.rs) keeps workspace, task and Strict checks. The model check becomes: equal to parent, **or** routing is enabled for that workspace and the helper's provider is allowed+trusted+enabled. Provisioning rechecks this, so a config change to disallow a provider stops new provisions of that helper.
- Agent row gets `route_reason TEXT` (migration, nullable). Shown under the helper's name in the Agents sidebar and returned by `agent_spawn`.

## Rate limit while running

When the gateway records a real rate-limit stop (429 that is not a concurrency retry, or quota `limit_reached`) for a routed helper's provider, the watcher asks the chooser again for that helper's job, excluding the limited provider. If it finds an allowed alternative, it switches the helper with the existing `set_model` path (allowed for routed helpers inside the allowed set) and records `route_reason` like `Switched: Anthropic hit its limit`, plus an activity event. Never switches Strict agents, never to a non-allowed provider. At most one switch per helper per 10 minutes.

## RPCs

- `routing.get {repo_id?}` -> `{config, source: "global"|"workspace"|"default", providers: [{id, name, trusted, enabled, usage_pct, limit_reached}]}`
- `routing.set {repo_id?, config|null}` -> validates (allowed ids exist, job refs well formed, pct 1..100); `null` with repo_id removes the override.
- `routing.preview {repo_id, parent_model, job}` -> `{model_ref, reason}` (for the settings screen's "what would happen now" line).

## Settings UI

Settings > "Helper models":
- Scope picker: All workspaces / a workspace (shows "Uses the global setting" until overridden, with Reset).
- On/off toggle. When off, one line: "Helpers use the same model as the agent that starts them."
- Allowed providers: checkboxes for trusted, enabled providers, each with its current usage percent. Text: "Helpers may send this workspace's code to the providers you tick."
- Model per job (Explore, Code, Verify): select with "Same as parent" first.
- "Avoid a provider above N% usage" number, "Verify on a different provider" toggle.
- Live preview line per job from `routing.preview`.
