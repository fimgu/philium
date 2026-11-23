# Philium

Philium is a desktop workspace for running coding agents on your own machine. You add your repositories, connect a model provider, and start agents that work in those repos. Philium shows every message, tool call and model request, asks before tools run (unless you tell it not to), and keeps an eye on secrets leaving your machine.

It is built with Tauri 2 and runs on macOS, Windows and Linux. The agent runtime is HAL, a separate project that Philium starts in the background and stops when you quit.

## Screenshots

Screenshots will be added here before the first public release.

<!-- Home with the Get set up card -->
<!-- An agent timeline -->
<!-- Memory -->

## Requirements

- Node.js and npm (a current LTS release)
- Rust and Cargo, installed with rustup
- The Tauri 2 system prerequisites for your platform (see the Tauri docs)
- A checkout of HAL, the agent runtime Philium bundles. Philium does not build without it.

Optional, for some features:

- The GitHub CLI (`gh`), for reviewing and publishing pull requests
- Docker or another container runtime, for sandboxes

## Build

Philium needs HAL next to it. By default the build script looks for a folder called `hal` beside your Philium checkout:

```
Developer/
  hal/
  philium/
```

If HAL lives somewhere else, set `HAL_DIR` to its path.

```sh
npm install
npm run sidecar          # builds HAL and stages it for Philium
npx tauri dev            # run in development mode
npx tauri build          # build an installer for your platform
```

`npm run sidecar` runs `scripts/sidecar.sh`. It builds HAL's desktop server from `HAL_DIR` (or `../hal`) and copies the binary into `src-tauri/binaries/`, named for your platform. Run it again whenever you update HAL.

Example with a custom location:

```sh
HAL_DIR=~/src/hal npm run sidecar
```

## First run

When Philium opens, the Home screen shows a **Get set up** card. Each row links to the screen where you finish that step and ticks itself off as you go. You can press **Later** to hide it, and **Check again** to refresh it.

Required:

1. **Sign in to a provider.** Open Providers and connect at least one model provider.
2. **Add a repository.** Open Repos and point Philium at a project you work on.
3. **Download local memory models.** Open Memory and start the download. These small models run on your machine so memory search works without sending your notes anywhere.
4. **Sign in to the GitHub CLI.** Install `gh` and run `gh auth login` in a terminal.

Optional:

- **Docker**, only needed for sandboxes.
- **Notifications**, to hear when an agent finishes or needs you.
- **Skills and MCP servers**, extra tools your agents can use.

Once the required rows are done, the card goes away on its own.

## Where data lives

- **App data**: Philium's own database, settings and stored secrets live in the standard app data folder for the id `dev.gunnarsson.philium`.
  - macOS: `~/Library/Application Support/dev.gunnarsson.philium`
  - Windows: `%APPDATA%\dev.gunnarsson.philium`
  - Linux: `~/.local/share/dev.gunnarsson.philium`
- **HAL configuration**: `~/.hal/hal.yaml`, or the file named by `HAL_CONFIG`.
- **Per repository**: an optional `.philium/` folder in the repo, for example `.philium/pipeline.yaml`. This belongs to the repo and is yours to commit or ignore.

Agents are not allowed to read Philium's own app data folder.

## Reset

To start fresh:

1. Quit Philium.
2. Delete the app data folder listed above for your platform.
3. Open Philium again. The Get set up card returns.

This removes Philium's settings, history, memory and stored secrets. It does not touch your repositories, any `.philium/` folders in them, or `~/.hal`. Remove those yourself if you want a completely clean slate.

## Status

Philium is in active development and not yet published. See `docs/PUBLIC-RELEASE.md` for what remains before it goes public.
