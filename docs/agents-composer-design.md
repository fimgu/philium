# Agents composer and transcript: competitor comparison and design targets

This compares the Agents workspace against three references: Claude Desktop (macOS), Codex (CLI and IDE/app) and Jcode (terminal). It describes what each one does well for an agent workflow and states what Philium has to match or deliberately do differently.

## Comparison

| Area | Claude Desktop | Codex | Jcode | Philium before | Philium target |
|---|---|---|---|---|---|
| Composer shape | Rounded card. Text first, with a toolbar row underneath. | CLI: single prompt line. App: card with a footer toolbar. | Single prompt line plus a status line. | Bare textarea with buttons floating beside it, plus a permanent disclaimer paragraph. | Card: images, then text, then one quiet toolbar row. No permanent disclaimer. |
| Add images | Paste, drop anywhere, and a "+" menu (files, screenshot). | CLI: paste image or `-i` path. App: attach button. | Paste. | Paste, plus a drop zone that never fires because the native webview intercepts drops. | Paste, drop over the whole agent pane, Attach menu (Choose images…, Take screenshot…). |
| Screenshot | Built-in capture into the composer. | None (manual file). | None. | None. | Native interactive capture of a region or window (Esc cancels). Lands straight in the draft. |
| Draft images | Thumbnails with a remove button on hover. | Placeholder chips `[image 1]`. | Chips. | 56 px images with a raw "x". | 64 px rounded thumbnails, named, with size, remove on hover and focus, and click to preview. |
| Limits and errors | Inline notice. | Error line. | Error line. | Toast. | Inline, persistent and specific ("Up to 8 images", "5 MiB"). Oversized screenshots are downscaled automatically to 2048 px. |
| Sent images in transcript | Thumbnail in the user turn, with a lightbox. | Placeholder. | Placeholder. | **Not shown at all.** | Lazily loaded thumbnails in both views, a lightbox, and a name chip in Terminal view. |
| Reasoning effort | Thinking toggle in the toolbar. | Low/Medium/High selector in the footer. | `/effort` command and a status line. | Three-button segmented control plus a disclaimer line. | Compact segmented control in the toolbar. The caveat moves into the tooltip. |
| Send / Stop | Single icon button that turns into Stop. | Same. | Esc to interrupt. | Text buttons. | One primary button that becomes Stop while busy. Enter sends, Shift+Enter adds a newline. |
| Draft state | Silent autosave. | n/a | n/a | "Saving draft… Keep this window open." flashes on every keystroke. | Silent while healthy. Only failures appear inline, with Retry. |
| Transcript default | Chat bubbles. | Terminal-style log. | Dense terminal transcript. | Conversation bubbles. | **Terminal transcript by default.** Conversation view stays one click away and is remembered. |
| Memory | n/a | n/a | n/a | Every keystroke rewrote a JSON file holding base64 images. Previews were data URLs (about 1.33× the bytes, held in the DOM). | Content-addressed blobs written once. The text index is tiny. Previews are object URLs from Blobs, revoked when removed. Base64 exists only while a send is in flight. The transcript cache is bounded (LRU of 48). |

## Deliberate differences
- Images are stored in Philium's private 0700 secrets directory, never in browser storage. Blob hashes are verified on every read.
- The transcript image reader is read-only. It serves only blobs referenced by a *user* message in the agent's *own* HAL session.
- Capture uses macOS `screencapture -i`. On first use macOS may ask for Screen Recording permission. If permission is denied, the capture shows only the desktop, and Philium cannot detect that.
