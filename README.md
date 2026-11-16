# Filium

A lightweight, cross-platform agent workspace (Tauri 2) with [HAL](HAL) as its harness.

- Bundles `hal-desktop-server` as a sidecar on a random loopback port with a per-launch token.
- The token stays in the Rust process; the web UI can only reach HAL through a path-checked `hal` command.
- Timeline shows every user message, model output, tool call (arguments + result) and token count per model request.
- Tool permissions: ask (approve/deny in the UI), auto-deny, or auto-allow.
- HAL shuts down when Filium quits (stdin EOF).

## Build

```sh
npm install
npm run sidecar          # builds ../hal and stages hal-desktop-server
npx tauri build          # .app/.dmg on macOS, .msi on Windows, .AppImage/.deb on Linux
npx tauri dev            # dev mode
```

Providers and models come from `~/.hal/hal.yaml` (or `HAL_CONFIG`).

## Roadmap

1. Outbound guard in HAL: scan/redact/block secrets before any model request; per-provider trust levels.
2. Network lock: only the configured provider is reachable during untrusted sessions.
3. Multi-session panes and terminal (Herdr replacement).
4. Signing, notarization, auto-update, Keychain for API keys.
