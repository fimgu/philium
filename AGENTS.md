# Philium agent guide

## UI verification and screenshots

- Use the visible Philium browser tab for UI walkthroughs and PR screenshots. Prefer a local preview with sample data so the user can follow along without exposing account details.
- For a fixture preview, run `npm run build -- --mode fixture`, then `node scripts/render/render.mjs --serve --port="$PHILIUM_PORT_BASE"`. Open the printed URL in the Philium browser and use its screenshot tool. Stop the server when finished.
- Headless runs are useful for automated regression checks. Say when a run uses a separate headless browser, and never use one to bypass a browser permission denial.

## Engineering rules

- **Always latest, never stale pins.** Philium must always use the latest versions, client fingerprints and model IDs. Resolve them at runtime (registry, live catalog, server response). Never hardcode a value that goes stale: a hardcoded `claude-cli/2.1.280` user agent once broke every Claude request.
  - Any unavoidable literal (fallback floor or security pin) lives only in `src-tauri/src/pins.rs`, with a written reason and how it is kept current. Reference it as `crate::pins::NAME`.
  - `pins::tests::no_stale_pins` scans `src-tauri/src/**/*.rs` and `src/**/*.{js,jsx}` (outside tests) for `name/1.2.3`, `client_version=`, `image:tag`, dated versions and `claude-`/`gpt-`/`gemini-` model IDs, and fails on anything not in `pins.rs`. Do not weaken it to make a change pass.
