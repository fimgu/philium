# Environments

Open **Environments → This checkout** to see recorded resources and their agent. Choose **Inspect local machine** to inspect named local Colima Docker sockets, including stopped containers. Expand a kind cluster, Compose project or the standalone group, then choose **View logs** for a specific container. Grouping is for navigation; labels and matching names do not prove ownership.

Machine scope is explicit. The explorer does not use a kubeconfig, switch contexts, follow redirects, execute in containers, publish logs or start/stop resources. Container metadata and logs are fetched from a fixed socket in the selected profile using GET only. Logs are a local 100-line snapshot capped at 64 KiB, with best-effort redaction, not a live stream or guaranteed secret scrub. A paused runtime, timeout or inaccessible socket is not an empty inventory. Select Refresh to retry. kind node logs are not Kubernetes pod logs. Disconnected profiles with no socket, non-Colima runtimes, cluster workloads, and historical sandbox runs are not inventoried here.

## Lifecycle contract

Visibility is not ownership. Existing and shared environments stay untouched. A managed temporary sandbox needs a creation record binding checkout, task, runtime identity and immutable resource IDs; an active lease; and a cleanup receipt. Task completion, cancellation or expiry should trigger idempotent cleanup of only its owned objects on the original runtime. Reconcile after restart. Reused names, missing ownership proof, a changed runtime or a live peer lease must prevent deletion. A shared Colima VM is not the cleanup unit. The explorer implements inspection, not this cluster reaper; pilot kind mutations remain disabled.

## Verification scope

macOS arm64: actual read-only backend against `exo-ship` returned 16 containers and 5 kind cluster labels. The first live attempt timed out at 5 seconds; on explicit retry the socket answered and inventory succeeded. No real container logs were opened. A disposable Unix socket checked the two GET paths, log cap, multiplexed frame decoding, missing endpoints, symlink rejection and rejection of delete/invalid ID. The built screen with a fixture bridge exercised grouping, opening and refreshing logs, switching profiles with a late response, and returning to checkout at 1280 and 1440 pixels. No disposable graphical session available: installed native app interaction and actual Kubernetes workload/cleanup lifecycle are not validated.
