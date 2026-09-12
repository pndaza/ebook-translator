# Ebook Translator

A macOS desktop app (Tauri 2 + Svelte 5) that translates whole books — **EPUB** and **PDF** — using the **Google AI Studio (Gemini) API**, and saves a clean EPUB.

## What it does

- Drop in an `.epub` or `.pdf` → the app parses it into sections and paragraphs
- Translates paragraph-by-paragraph in batches with a Gemini Flash model
- Saves an EPUB in one of two modes (selectable per book):
  - **Bilingual** — the original paragraph followed by its translation (formatting, images, and structure of the original book are preserved)
  - **Translation only** — the translated text replaces the original
- Pauses/resumes: finished batches are cached on disk, so an interrupted run continues where it left off
- 429/5xx errors retry automatically with backoff; failed batches keep the original text

## Setup

```sh
pnpm install
pnpm tauri dev      # development
pnpm tauri build    # release .app/.dmg
```

1. Get an API key at [aistudio.google.com/apikey](https://aistudio.google.com/apikey)
2. Open ⚙ Settings in the app, paste the key, press **Test key**
3. Drop a book, pick the target language (defaults to Burmese), press **Translate**

The key is stored only in the app's local settings file. Requests are sent with `store: false`, so translated content is not retained by the API.

## Architecture

```
src/               Svelte 5 UI (drop zone → book overview → progress TOC)
src-tauri/src/
  epub/            container/OPF/spine parsing, lol_html block collect/rewrite, zip repack
  pdf.rs           per-page text extraction → chapter documents (pdf-extract + lopdf)
  gemini.rs        Gemini Interactions API client (structured output, retries)
  job.rs           batch pipeline: worker pool, resume log, progress events
  commands.rs      Tauri commands (inspect/start/cancel/save/settings)
```

- Blocks (`p, h1–h6, li, blockquote, td, th, dd, dt, figcaption`) are collected per document; digits-only/too-short blocks are skipped
- Batches are token-budgeted so each book costs as few free requests as possible: ~5K estimated input tokens per request for Flash-Lite, ~10K for Flash (which has ~25× fewer free requests); ≤100 paragraphs per batch
- 3 parallel requests for every model, paced automatically to its free-tier rate limit (Flash-Lite 500/min, Flash 20/min)
- On a 429 the whole job automatically slows down and retries with long delays (up to 12 times, honoring Retry-After), so rate limits never fail a batch; 5xx/network errors retry with exponential backoff
- Resume cache: `~/Library/Application Support/com.pndaza.ebook-translator/jobs/<hash-model-lang>.jsonl`
- Tests: `cd src-tauri && cargo test` (28 tests incl. full EPUB round-trip)
- Debug a book pipeline offline: `INSPECT_PATH=<file> cargo test debug_inspect -- --nocapture`

## Notes & limits

- Scanned PDFs (image-only) are not supported — no OCR
- PDF output is a freshly built EPUB (the app never writes PDFs)
- Default model `gemini-3.5-flash-lite`; dropdown covers the current Flash family + 3.1 Pro preview

## Browser preview (UI development)

`pnpm dev` serves the UI standalone with fixture data (no Tauri, no API) — useful for
working on components in a normal browser.

## Releases & self-update (macOS)

`git push` a `v*` tag (matching the version in `src-tauri/tauri.conf.json`) to run the
release workflow: it builds a universal (Apple Silicon + Intel) app and attaches the
DMG plus updater artifacts to a draft GitHub release — review and publish it. The app
then offers **Settings → App updates → Check for updates** and installs signed
updates from the published `latest.json`.

- Update signing uses the minisign keypair at `~/.tauri/ebook-translator.key`; the
  private key lives only in the `TAURI_SIGNING_PRIVATE_KEY` repo secret (never in git)
- Local `tauri build` bundles need those env vars set; `tauri dev` and `pnpm build`
  do not

### First launch: Gatekeeper

The app is ad-hoc signed (no Apple notarization), so macOS may warn that it "cannot
verify the developer". After moving the app to `/Applications`, either right-click it
and choose **Open → Open**, or clear the quarantine flag in Terminal:

```sh
xattr -cr /Applications/ebook-translator.app
```

(`sudo xattr -rd com.apple.quarantine /Applications/ebook-translator.app` does the
same for just the quarantine attribute.)
