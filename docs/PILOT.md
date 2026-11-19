# macOS side-by-side pilot

Build from the repository root with `npm run pilot:build` after dependencies are cached locally. This produces an optimized release binary with thin link-time optimization and two Cargo jobs. It does not change the regular Philium release profile. The local ad-hoc signed artifact is `src-tauri/target/release/bundle/macos/Philium Pilot.app`. Do not replace the installed Philium. The separate bundle identifier is `dev.gunnarsson.philium.pilot`.

Normal pilot startup uses `~/Philium Pilot/{data,workspace,home}`. This is profile separation, not an OS security boundary. The pilot skips automatic personal-library import, host reconciliation, ledger-driven shutdown cleanup, and self-update. In-memory agent, terminal and desk handles retain their normal shutdown paths. Keeping a pilot checkout does not grant access to a remote runner or recipient.

## Daily use

The Pilot is meant to be worked in, not only inspected. What that needed, and what it means for you:

* **Model providers.** Provider keys and sign-ins are stored in the Pilot's own vault, `~/Philium Pilot/data/secrets/vault.json`, readable only by you. Agents cannot read that folder. Which provider receives a checkout's code is your decision each time you start an agent; the Review screen's "Context: who can see this?" lists the recipients for the selected checkout.
* **Your terminals.** A terminal you open is your own login shell with your own home, so your shell profile, toolchain and cluster settings load. Agent shells, HAL and everything a model drives keep the Pilot's private home. Variables named `JCODE_*` or `HERDR_*` and a `~/.jcode` directory on `PATH` are dropped when the Pilot starts, for the Pilot and every child.
* **What Strict agents cannot read.** The private folders of your account home (`~/.ssh`, cloud and Git credentials, other agent tools' data, personal folders), the Pilot's database and other agents' sessions, and the vault. A registered checkout and granted folders stay readable.
* **One coordinator per checkout.** Select a checkout and open Home: **Start coordinator** starts an orchestrator for that checkout, or **Open coordinator** opens the existing one. Tell it what you want; it creates tasks and starts helpers in the same checkout. A coordinator cannot reach another checkout.
* **Who owns a task.** Handing a task to an agent, from the New agent dialog, by a coordinator, or with **Hand to agent** on the task, is recorded at once and shown on the board. It survives Stop, quit and a crash: the board then says "Handed to NAME · stopped", and **Resume** makes the agent hold the task again. **Release** hands the task back for good. A running agent keeps its task without having to renew anything.
* **Reports and redirects.** Mail from you, from an agent's lead and from its own helpers is delivered into the agent's conversation when it is idle, at most 30 times an hour per agent. Mail between unrelated agents waits in the inbox.
* **Removing an agent** deletes its own worktree and scratch after confirmation when there is unsaved work. The Pilot still refuses to remove an agent that recorded a container, cluster or VM; release those yourself first.
* **After a quit or crash** every agent is stopped. Nothing restarts on its own and an interrupted turn is not replayed. Open the agent and press **Resume**; the conversation continues in the same session.
* **Workday.** The Workday screen lists each action you took on a screen, by name, checkout and outcome only. Record a fallback there every time you use Jcode, Herdr, a manual database edit or outside help; Philium cannot see that by itself. **Export summary** writes a file under `~/Philium Pilot/data/workday/`.

## Check an installed bundle through its own window

`python3 scripts/check-acceptance.py '/path/to/Philium Pilot.app'` starts the bundle's executable with a throwaway home and `--pilot-ui-script`. The window is hidden and cannot take focus. Each scenario clicks, types and reads the rendered page; the bundle refuses scripts that reach for the backend bridge, so the harness cannot perform a step the interface lacks. Model, origin, deployment observer, client programs and checkouts are local stand-ins. It needs no other agent tool; set `PHILIUM_SCRATCH_DIR` to choose where the throwaway folder goes. It does not cover a visible window, the real profile, a real model, or a workday.

### With a real model

A stand-in model cannot show a fault in how requests are shaped for a real provider. One such fault, a Claude request refused from the second message on, passed every stand-in check. After you have signed in to Claude in the Pilot yourself, `python3 scripts/check-real-model.py '/path/to/Philium Pilot.app'` runs the coordinator flow on that sign-in: start a coordinator, delegate in plain words, read the board, redirect, interrupt, stop, kill the app in the middle of a command, reopen and resume. It copies the provider rows and the short-lived access token into a throwaway profile, never the refresh token and none of your checkouts, agents or tasks, and deletes that profile when it ends. It spends about fifty short requests of your subscription on the chosen model (`--model`, default `claude-sonnet-5-5`).

Both hidden harnesses share one trap: while a hidden instance runs from a bundle, opening that same bundle from the Dock, Finder or Spotlight does nothing, because macOS hands the request to the hidden instance. Run them on a copy of the bundle (`ditto`) whenever someone may open the app.

## Reproduce without opening a window

With `PHILIUM_SCRATCH_DIR` pointing to a writable scratch directory, run `python3 scripts/check-pilot.py 'src-tauri/target/release/bundle/macos/Philium Pilot.app'`. The script checks the bundle ID, signature and sidecar, invokes the packaged executable directly with a disposable HOME, and removes its fixtures. It never uses LaunchServices. The first process registers two local Git repositories with the same basename and creates a different task in each. Two fresh processes must resolve both saved IDs, see only the matching tasks and retrieve the original HAL session. It also checks startup/cleanup guards, refused duplicate seeds and empty recoveries, and a symlinked profile root.

## What remains untested

For the daily loop, select a registered checkout and open Home. Its unfinished tasks, agents, worktrees and unreleased agent resources are fetched by registered ID. The resource list is the local ledger, not a pod health monitor. Use **Open terminals** to run the project's existing commands yourself, **Review changes** for the code view, and **Review evidence** for saved packets. A newly opened checkout terminal records its repo ID; pre-existing human terminals with no ID stay available under All Workspaces. Neither reading an overview nor switching repos starts a command or sends data to a model. The pilot refuses its built-in kind create/delete operations, including calls from agent tools. This is not a sandbox for commands a human explicitly runs in a shell.

`check-pilot.py` starts the real packaged HAL and calls backend RPC, not the window. `check-acceptance.py` uses the real window, hidden. Neither covers a visible window's focus, the graphical quit menu, or rollback in a separate login. Exo/Sentinel responsiveness over a workday, explicit recipient/payload grants, bounded local execution and approved private nightlies remain outside these checks.
### Task-owned local sandboxes

In **Environments → This checkout → New task sandbox**, choose a claimed task and its agent, a local Colima Docker profile, a command and a time limit. Preview the command before starting. Open the task row for bounded logs or confirm release of that container. The Alpine 3.20 image must already be cached. The command has no checkout mount or external network. A sandbox is explicitly issued, not inferred from anything visible in the machine inspector.

The app records the lease before creation. While running it checks for ended tasks, expired leases and interrupted starts. When the app is closed, the command has an in-container time limit, but container deletion and its receipt require Philium to run again. A changed runtime or mismatched identity blocks deletion. This path never deletes kind clusters or Colima VMs.

`python3 scripts/check-managed.py <local-profile>` exercises the backend RPC with disposable checkouts, agents and real containers. It checks fresh-process recovery, cancellation, expiry, identity refusals, cleanup retry and a reconstructed interrupted issuance. It needs `PHILIUM_SCRATCH_DIR` and never pulls the image. The headless renderer tests preview, logs and release with fixture RPC responses. Neither check establishes native quit/reopen, responsiveness, rollback or a full workday.
