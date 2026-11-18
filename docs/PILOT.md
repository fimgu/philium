# macOS side-by-side pilot

Build from the repository root with `npm run pilot:build` after dependencies are cached locally. This produces an optimized release binary with thin link-time optimization and two Cargo jobs. It does not change the regular Philium release profile. The local ad-hoc signed artifact is `src-tauri/target/release/bundle/macos/Philium Pilot.app`. Do not replace the installed Philium. The separate bundle identifier is `dev.gunnarsson.philium.pilot`.

Normal pilot startup uses `~/Philium Pilot/{data,workspace,home}`. This is profile separation, not an OS security boundary. Do not add private repositories, configure an external provider or run unapproved tools for this trial. The pilot skips automatic personal-library import, host reconciliation, ledger-driven shutdown cleanup, and self-update. In-memory agent, terminal and desk handles retain their normal shutdown paths. Keeping a pilot checkout does not grant access to a remote runner or recipient.

## Reproduce without opening a window

With `JCODE_SCRATCH_DIR` pointing to a writable scratch directory, run `python3 scripts/check-pilot.py 'src-tauri/target/release/bundle/macos/Philium Pilot.app'`. The script checks the bundle ID, signature and sidecar, invokes the packaged executable directly with a disposable HOME, and removes its fixtures. It never uses LaunchServices. The first process registers two local Git repositories with the same basename and creates a different task in each. Two fresh processes must resolve both saved IDs, see only the matching tasks and retrieve the original HAL session. It also checks startup/cleanup guards, refused duplicate seeds and empty recoveries, and a symlinked profile root.

## What remains untested

For the daily loop, select a registered checkout and open Home. Its unfinished tasks, agents, worktrees and unreleased agent resources are fetched by registered ID. The resource list is the local ledger, not a pod health monitor. Use **Open terminals** to run the project's existing commands yourself, **Review changes** for the code view, and **Review evidence** for saved packets. A newly opened checkout terminal records its repo ID; pre-existing human terminals with no ID stay available under All Workspaces. Neither reading an overview nor switching repos starts a command or sends data to a model. The pilot refuses its built-in kind create/delete operations, including calls from agent tools. This is not a sandbox for commands a human explicitly runs in a shell.

This path starts the real packaged HAL and calls backend RPC, not the Tauri window, event loop or graphical quit sequence. Next use a disposable graphical login for repository switching, quit/reopen twice and rollback; do not automate the user's live desktop. Exo/Sentinel responsiveness over a workday, explicit recipient/payload grants, bounded local execution, independent base/candidate findings and approved private nightlies remain outside this check.
