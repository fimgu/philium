# Contributing to Philium

Thanks for your interest in Philium. This guide covers how to set up, make a change and send it in.

## Setup

Follow [Requirements](README.md#requirements) and [Build](README.md#build) in the README. You need a HAL checkout next to Philium, or `HAL_DIR` pointing at one.

## Making a change

1. Open an issue first for anything larger than a small fix, so we can agree on the approach.
2. Create a branch from `main`.
3. Keep changes focused. One topic per pull request.
4. Run the Rust tests with `cargo test` in `src-tauri/` and make sure `npm run build` succeeds.
5. Open a pull request and fill in the template.

## Engineering rules

Read [AGENTS.md](AGENTS.md). In short, Philium always uses the latest versions, client fingerprints and model IDs, resolved at runtime. Any unavoidable literal lives in `src-tauri/src/pins.rs` with a written reason. The `no_stale_pins` test enforces this. Do not weaken it.

## Reporting bugs and requesting features

Use the issue templates. For security problems, do not open a public issue. Contact the maintainer privately instead.

## Code of Conduct

By taking part you agree to follow the [Code of Conduct](CODE_OF_CONDUCT.md).
