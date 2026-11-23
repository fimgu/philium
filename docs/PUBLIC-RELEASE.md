# Before Philium goes public

Philium stays private until every item below is done.

- [ ] **HAL is public.** Philium cannot be built without it. Link its public URL in `README.md` (Requirements and Build).
- [ ] **License chosen by the owner.** Add a `LICENSE` file and update `license` in `package.json` (currently `ISC` as an npm default) and `src-tauri/Cargo.toml`. Agents must not pick one.
- [ ] **Internal docs reviewed.** Decide for each whether to publish, rewrite or move out of the repo:
  - [ ] `docs/gaps/`
  - [ ] `docs/LANES.md`
  - [ ] `docs/GAPS.md`
  - [ ] Other planning and review notes in `docs/` and `docs/shots/`
- [ ] **Secrets scan.** Scan the full git history, not just the current tree (for example with gitleaks or trufflehog). Rotate anything found, then rewrite history or start a fresh public repo.
- [ ] **Personal details checked.** Look for local paths, usernames, emails and private hostnames in code, docs, tests and fixtures.
- [ ] **CI.** Add a workflow that installs dependencies, builds HAL as the sidecar, runs the frontend and Rust tests (including `pins::tests::no_stale_pins`) and builds on macOS, Windows and Linux.
- [ ] **README screenshots.** Replace the placeholder section.
- [ ] **Flip visibility** only after the items above are checked.
