<script lang="ts">
  import { save } from "@tauri-apps/plugin-dialog";
  import { openPath, revealItemInDir } from "@tauri-apps/plugin-opener";
  import { cancelJob, saveOutput, startJob, formatChars } from "$lib/api";
  import { app, resetBook } from "$lib/stores.svelte";

  let saving = $state(false);
  let startingFull = $state(false);
  let confirmDiscard = $state(false);
  let confirmEl: HTMLDivElement | null = $state(null);

  const progress = $derived(app.progress);
  const sample = $derived(progress?.sample ?? false);
  const pct = $derived(
    progress && progress.charsTotal > 0
      ? Math.min(100, Math.round((progress.charsDone / progress.charsTotal) * 100))
      : 0,
  );
  const done = $derived(progress?.status === "completed" || progress?.status === "partial");
  const running = $derived(progress?.status === "running");
  const headline = $derived(
    progress?.status === "completed"
      ? sample
        ? "Sample ready"
        : "Done"
      : progress?.status === "partial"
        ? "Done — with gaps"
        : progress?.status === "cancelled"
          ? "Paused"
          : progress?.status === "failed"
            ? "Failed"
            : (progress?.status ?? ""),
  );

  async function saveEpub() {
    if (!app.book) return;
    const lang = app.jobLang || (app.form?.lang?.split(" (")[0] ?? "translation");
    const safeTitle = app.book.title.replace(/[\\/:*?"<>|]/g, "").trim() || "book";
    const path = await save({
      defaultPath: `${safeTitle} (${lang}${sample ? ", sample" : ""}).epub`,
      filters: [{ name: "EPUB", extensions: ["epub"] }],
    });
    if (!path) return;
    saving = true;
    try {
      const res = await saveOutput(path);
      app.savedPath = res.path;
    } catch (e) {
      app.error = String(e);
    } finally {
      saving = false;
    }
  }

  // The sample's segments live in the translation cache, so the full run
  // reuses them and only pays for the rest of the book. After a webview
  // reload `lastOptions` is gone, so fall back to the progress snapshot's
  // own job parameters.
  async function translateFull() {
    const fromSnapshot = progress
      ? {
          targetLang: progress.targetLang,
          mode: progress.mode,
          model: progress.model,
          customInstructions: app.settings?.customInstructions ?? "",
          sample: false,
        }
      : null;
    const opts = app.lastOptions ?? fromSnapshot;
    if (!opts) {
      app.view = "ready";
      return;
    }
    startingFull = true;
    app.error = "";
    try {
      const full = { ...opts, sample: false };
      await startJob(full);
      app.lastOptions = full;
      app.progress = null;
      app.logs = [];
      app.savedPath = "";
    } catch (e) {
      app.error = String(e);
    } finally {
      startingFull = false;
    }
  }

  async function reveal() {
    if (app.savedPath) await revealItemInDir(app.savedPath).catch(() => openPath(app.savedPath));
  }

  // Cancelling out of a finished job must never silently throw away an
  // unsaved translation. A finished sample is cheap to redo, though.
  function cancel() {
    if (done && !sample && !app.savedPath) {
      confirmDiscard = true;
      return;
    }
    resetBook();
  }

  function keepBook() {
    confirmDiscard = false;
  }

  function discardBook() {
    confirmDiscard = false;
    resetBook();
  }

  function onKeydown(e: KeyboardEvent) {
    if (!confirmDiscard) return;
    if (e.key === "Escape") {
      e.preventDefault();
      keepBook();
    }
  }

  // Move focus onto the dialog when it opens.
  $effect(() => {
    if (confirmDiscard) confirmEl?.focus();
  });
</script>

{#if progress}
  <article class="card">
    <header>
      <div>
        <p class="kind">
          {done
            ? sample
              ? "Sample"
              : "Translated"
            : progress.status === "failed"
              ? "Translation failed"
              : sample
                ? "Sampling"
                : "Translating"}
          — {app.book?.title ?? ""}
        </p>
        <h2>{headline}</h2>
      </div>
      <div class="numbers">
        <span class="mono">{progress.batchesDone}/{progress.batchesTotal} batches</span>
        {#if progress.cachedSegments}
          <span class="mono">{progress.cachedSegments.toLocaleString()} from cache</span>
        {/if}
        {#if progress.tokensUsed}
          <span class="mono">{progress.tokensUsed.toLocaleString()} tokens</span>
        {/if}
        <span class="mono">{formatChars(progress.charsDone)} / {formatChars(progress.charsTotal)}</span>
      </div>
    </header>

    <div
      class="rule"
      role="progressbar"
      aria-valuemin="0"
      aria-valuemax="100"
      aria-valuenow={pct}
      class:indeterminate={running && progress.charsTotal === 0}>
      <div class="fill" style="width: {pct}%"></div>
    </div>

    {#if progress.error}
      <p class="error" role="alert">{progress.error}</p>
    {/if}
    {#if app.error}
      <p class="error" role="alert">{app.error}</p>
    {/if}

    <ul class="toc">
      {#each progress.segments as s (s.id)}
        <li class="row" class:done={s.state === "done"} class:active={s.state === "active"}>
          <span class="title" title={s.title}>{s.title}</span>
          <span class="leader" aria-hidden="true"></span>
          <span class="count mono">{s.done}/{s.total}</span>
          <span class="mark" aria-label={s.state}>
            {#if s.state === "done"}✓{:else if s.state === "active"}<span class="pulse"></span>{:else}·{/if}
          </span>
        </li>
      {/each}
    </ul>

    {#if app.logs.length}
      <details class="logs">
        <summary>Activity</summary>
        {#each app.logs as line}
          <p>{line}</p>
        {/each}
      </details>
    {/if}

    <footer>
      {#if running}
        <div class="btn-group">
          <button class="btn btn-ghost" onclick={() => cancelJob()}>Pause</button>
        </div>
      {:else if done}
        {#if sample}
          {#if app.savedPath}
            <p class="saved">Saved to <span class="mono path">{app.savedPath}</span></p>
          {:else}
            <p class="hint">Everything the sample translated is reused when you run the full book.</p>
          {/if}
          <div class="btn-group">
            {#if app.savedPath}
              <button class="btn btn-ghost" onclick={reveal}>Show in Finder</button>
              <button class="btn btn-ghost" onclick={resetBook}>Done</button>
            {:else}
              <button class="btn btn-ghost" onclick={cancel}>Cancel</button>
              <button class="btn btn-ghost" onclick={saveEpub} disabled={saving}>
                {saving ? "Saving…" : "Save sample"}
              </button>
            {/if}
            <button class="btn btn-primary" onclick={translateFull} disabled={startingFull}>
              {startingFull ? "Starting…" : "Translate full book"}
            </button>
          </div>
        {:else if app.savedPath}
          <p class="saved">Saved to <span class="mono path">{app.savedPath}</span></p>
          <div class="btn-group">
            <button class="btn btn-ghost" onclick={reveal}>Show in Finder</button>
            <button class="btn btn-ghost" onclick={resetBook}>Done</button>
          </div>
        {:else}
          <div class="btn-group">
            <button class="btn btn-ghost" onclick={cancel}>Cancel</button>
            <button class="btn btn-primary" onclick={saveEpub} disabled={saving}>
              {saving ? "Saving…" : "Save book"}
            </button>
          </div>
        {/if}
      {:else if progress.status === "cancelled" || progress.status === "failed"}
        <p class="hint">Progress is kept on this machine — start again to continue where you left off.</p>
        <div class="btn-group">
          <button class="btn btn-primary" onclick={() => (app.view = "ready")}>Back to book</button>
        </div>
      {/if}
      {#if progress.batchesFailed > 0 && done}
        <p class="warn">{progress.batchesFailed} batches failed — their paragraphs stay in the original language.</p>
      {/if}
    </footer>
  </article>

  {#if confirmDiscard}
    <div class="overlay" role="presentation">
      <button class="overlay-close" aria-label="Keep the book" onclick={keepBook}></button>
      <div
        class="confirm"
        role="alertdialog"
        aria-modal="true"
        aria-labelledby="discard-title"
        tabindex="-1"
        bind:this={confirmEl}>
        <h3 id="discard-title">Unsaved translation</h3>
        <p>“{app.book?.title}” hasn't been saved yet — discard the translated book?</p>
        <footer>
          <button class="btn btn-ghost" onclick={keepBook}>Keep book</button>
          <button class="btn btn-danger" onclick={discardBook}>Discard</button>
        </footer>
      </div>
    </div>
  {/if}
{/if}

<svelte:window onkeydown={onKeydown} />

<style>
  .card {
    width: min(680px, 100%);
    background: var(--paper);
    border-radius: var(--radius);
    box-shadow: 0 14px 40px rgba(6, 12, 18, 0.45);
    padding: 24px 28px 20px;
    display: flex;
    flex-direction: column;
    gap: 14px;
  }
  header {
    display: flex;
    justify-content: space-between;
    align-items: flex-end;
    gap: 14px;
  }
  .kind {
    font-size: 10.5px;
    font-weight: 700;
    letter-spacing: 0.14em;
    text-transform: uppercase;
    color: var(--gilt-deep);
    max-width: 340px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  h2 {
    font-family: var(--font-display);
    font-size: 22px;
    font-weight: 560;
    text-transform: capitalize;
  }
  .numbers {
    display: flex;
    gap: 8px;
    flex-wrap: wrap;
    justify-content: flex-end;
  }
  .mono {
    font-family: var(--font-mono);
    font-size: 11px;
    color: var(--ink);
    background: var(--paper-dim);
    padding: 2px 6px;
    border-radius: 4px;
  }

  .rule {
    height: 5px;
    border-radius: 999px;
    background: var(--paper-dim);
    overflow: hidden;
  }
  .fill {
    height: 100%;
    background: linear-gradient(90deg, var(--gilt-deep), var(--gilt-bright));
    border-radius: 999px;
    transition: width 0.4s ease;
  }
  .rule.indeterminate .fill {
    width: 30% !important;
    animation: slide 1.2s ease-in-out infinite alternate;
  }
  @keyframes slide {
    from {
      transform: translateX(-40%);
    }
    to {
      transform: translateX(300%);
    }
  }

  /* the signature: a real table of contents with dot leaders */
  .toc {
    list-style: none;
    padding: 4px 0;
    margin: 0;
    max-height: 300px;
    overflow-y: auto;
    user-select: text;
  }
  .row {
    display: flex;
    align-items: baseline;
    gap: 8px;
    padding: 3.5px 0;
    font-size: 13.5px;
  }
  .title {
    color: var(--ink-soft);
    max-width: 55%;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .row.done .title {
    color: var(--ink);
  }
  .leader {
    flex: 1;
    border-bottom: 2px dotted var(--paper-edge);
    transform: translateY(-3px);
  }
  .row.done .leader {
    border-bottom-color: var(--gilt);
  }
  .count {
    visibility: hidden;
  }
  .row.active .count,
  .row.done .count {
    visibility: visible;
  }
  .mark {
    width: 14px;
    text-align: center;
    color: var(--ink-faint);
    font-weight: 700;
  }
  .row.done .mark {
    color: var(--ok);
  }
  .pulse {
    display: inline-block;
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--gilt);
    animation: pulse 1.1s ease-in-out infinite;
  }
  @keyframes pulse {
    0%,
    100% {
      opacity: 0.35;
    }
    50% {
      opacity: 1;
    }
  }

  .logs {
    border-top: 1px dashed var(--paper-edge);
    padding-top: 8px;
    font-family: var(--font-mono);
    font-size: 11px;
    color: var(--ink-soft);
  }
  .logs summary {
    cursor: pointer;
    user-select: none;
    letter-spacing: 0.06em;
    text-transform: uppercase;
    font-size: 10px;
    font-weight: 700;
    color: var(--ink-faint);
  }
  .logs p {
    margin-top: 4px;
    user-select: text;
  }

  footer {
    display: flex;
    align-items: center;
    gap: 12px;
    flex-wrap: wrap;
    border-top: 1px solid var(--paper-edge);
    padding-top: 14px;
  }
  .btn-group {
    display: flex;
    align-items: center;
    gap: 10px;
    margin-left: auto;
  }
  .saved {
    font-size: 12.5px;
    color: var(--ink-soft);
    flex: 1;
    min-width: 0;
  }
  .path {
    font-size: 10.5px;
    word-break: break-all;
  }
  .hint {
    flex: 1;
    font-size: 12px;
    color: var(--ink-soft);
  }
  .error {
    color: var(--error);
    font-size: 13px;
  }
  .warn {
    width: 100%;
    font-size: 12px;
    color: var(--gilt-deep);
  }

  /* in-app discard confirmation */
  .overlay {
    position: fixed;
    inset: 0;
    z-index: 50;
    background: rgba(10, 16, 22, 0.62);
    display: grid;
    place-items: center;
  }
  .overlay-close {
    position: absolute;
    inset: 0;
    width: 100%;
    cursor: default;
  }
  .confirm {
    position: relative;
    width: min(380px, 92vw);
    background: var(--paper);
    border-radius: var(--radius);
    box-shadow: 0 24px 60px rgba(4, 8, 12, 0.55);
    padding: 20px 22px 16px;
    outline: none;
  }
  .confirm h3 {
    font-family: var(--font-display);
    font-size: 18px;
    font-weight: 560;
    margin-bottom: 6px;
  }
  .confirm p {
    font-size: 13px;
    color: var(--ink-soft);
  }
  .confirm footer {
    display: flex;
    justify-content: flex-end;
    gap: 10px;
    margin-top: 16px;
    padding-top: 12px;
    border-top: 1px solid var(--paper-edge);
  }
  .btn-danger {
    color: var(--error);
    border: 1px solid var(--paper-edge);
    background: transparent;
  }
  .btn-danger:hover {
    border-color: var(--error);
    background: #f5e7e2;
  }
</style>
