<script lang="ts">
  import {
    startJob,
    formatChars,
    estimateRequests,
    LANGUAGES,
    MODELS,
    MODEL_RATE_HINT,
  } from "$lib/api";
  import { app, resetBook } from "$lib/stores.svelte";

  let { onstart }: { onstart: () => void } = $props();

  // Seed once: carry over choices from a previous visit of this form if any,
  // otherwise fall back to the saved defaults. The $effect below syncs the
  // store as the user edits (and on first mount), so "Back to book" keeps
  // their language — and the language-keyed resume cache stays valid.
  const seeded = app.form ?? {
    lang: LANGUAGES[0],
    mode: "translated",
    model: app.settings?.model || "gemini-3.5-flash-lite",
  };
  // The language picker offers two options; anything left over from an older
  // default (or a free-typed settings value) falls back to Burmese.
  if (!LANGUAGES.includes(seeded.lang)) {
    seeded.lang = LANGUAGES[0];
  }

  let lang = $state(seeded.lang);
  let mode = $state(seeded.mode);
  let model = $state(seeded.model);
  let starting = $state(false);
  let requests = $state<number | null>(null);

  // Re-run the real batcher for the loaded book whenever the model changes.
  $effect(() => {
    const m = model;
    estimateRequests(m)
      .then((n) => {
        if (m === model) requests = n;
      })
      .catch(() => {
        if (m === model) requests = null;
      });
  });

  $effect(() => {
    app.form = { lang, mode, model };
  });

  async function begin() {
    if (!app.book) return;
    starting = true;
    app.error = "";
    try {
      await startJob({
        targetLang: lang,
        mode,
        model,
        customInstructions: app.settings?.customInstructions ?? "",
      });
      // Drop the previous run's card so the new one starts clean.
      app.progress = null;
      app.logs = [];
      app.savedPath = "";
      app.jobLang = lang;
      app.view = "running";
      onstart();
    } catch (e) {
      app.error = String(e);
    } finally {
      starting = false;
    }
  }

  const book = $derived(app.book);
  const noKey = $derived(!app.settings?.apiKey);
</script>

{#if book}
  <article class="card">
    <div class="split">
      <div class="overview">
        {#if book.coverDataUrl}
          <img class="cover" src={book.coverDataUrl} alt="" />
        {:else}
          <div class="cover blank"><span>{book.format.toUpperCase()}</span></div>
        {/if}
        <h2>{book.title}</h2>
        <p class="byline">
          {#if book.author}{book.author} · {/if}{book.format === "pdf" ? "PDF" : "EPUB"}
        </p>
        <p class="stats">
          {book.segments.length}
          {book.segments.length === 1 ? "section" : "sections"} ·
          {book.segments.reduce((n, s) => n + s.blocks, 0)} paragraphs ·
          {formatChars(book.totalChars)}
        </p>
        {#if book.warnings.length}
          <p class="warn" role="alert">
            {#each book.warnings as w}{w} {/each}
          </p>
        {/if}
      </div>

      <div class="form">
        <div class="field">
          <label for="lang">Translate into</label>
          <select id="lang" bind:value={lang}>
            {#each LANGUAGES as l}<option value={l}>{l}</option>{/each}
          </select>
        </div>

        <div class="field">
          <label for="model">Model</label>
          <select id="model" bind:value={model}>
            {#each MODELS as m}<option value={m.id}>{m.label}</option>{/each}
          </select>
          <p class="rate-hint">{MODEL_RATE_HINT}</p>
          {#if requests !== null}
            <p class="estimate">≈ {requests} requests for this book</p>
          {/if}
        </div>

        <div class="field">
          <span class="label" id="output-label">Output</span>
          <div class="segmented" role="radiogroup" aria-labelledby="output-label">
            <button
              type="button"
              role="radio"
              class:on={mode === "translated"}
              aria-checked={mode === "translated"}
              onclick={() => (mode = "translated")}>
              Translation only
            </button>
            <button
              type="button"
              role="radio"
              class:on={mode === "bilingual"}
              aria-checked={mode === "bilingual"}
              onclick={() => (mode = "bilingual")}>
              Bilingual
            </button>
          </div>
        </div>
      </div>
    </div>

    {#if app.error}
      <p class="error" role="alert">{app.error}</p>
    {/if}

    <footer class="actions">
      {#if noKey}
        <p class="hint">
          No API key yet — <button class="linkish" onclick={() => (app.settingsOpen = true)}>add your Google AI Studio key</button> to start.
        </p>
      {/if}
      <div class="btn-group">
        <button class="btn btn-ghost" disabled={starting} onclick={resetBook}>Cancel</button>
        <button class="btn btn-primary" disabled={noKey || starting || !lang.trim()} onclick={begin}>
          {starting ? "Starting…" : `Translate to ${lang.split(" (")[0]}`}
        </button>
      </div>
    </footer>
  </article>
{/if}

<style>
  .card {
    width: min(690px, 100%);
    background: var(--paper);
    border-radius: var(--radius);
    box-shadow: 0 14px 40px rgba(6, 12, 18, 0.45);
    padding: 26px 28px 20px;
  }
  .split {
    display: grid;
    grid-template-columns: 168px 1fr;
    gap: 14px 30px;
    align-items: start;
  }

  /* left: the book itself */
  .overview {
    padding-right: 6px;
  }
  .cover {
    display: block;
    width: 100%;
    margin-bottom: 14px;
    object-fit: cover;
    border-radius: 4px;
    box-shadow: 2px 3px 8px rgba(30, 25, 15, 0.3), 1px 0 0 rgba(0, 0, 0, 0.12) inset;
    background: var(--paper-dim);
  }
  .cover.blank {
    aspect-ratio: 2 / 3;
    display: grid;
    place-items: center;
    background: #efe8d9;
    font-family: var(--font-mono);
    font-size: 11px;
    letter-spacing: 0.14em;
    color: var(--ink-faint);
    border: 1px solid var(--paper-edge);
    box-shadow: none;
  }
  h2 {
    font-family: var(--font-display);
    font-size: 21px;
    font-weight: 560;
    line-height: 1.25;
    margin: 0 0 4px;
  }
  .byline {
    color: var(--ink-soft);
    font-size: 12.5px;
  }
  .stats {
    margin-top: 8px;
    color: var(--ink-soft);
    font-size: 12px;
    line-height: 1.6;
  }
  .warn {
    margin-top: 8px;
    color: var(--gilt-deep);
    font-size: 12px;
  }

  /* right: the translation job */
  .form {
    display: flex;
    flex-direction: column;
    gap: 16px;
    padding-top: 2px;
  }
  .field {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  label,
  .label {
    font-size: 11px;
    font-weight: 700;
    letter-spacing: 0.09em;
    text-transform: uppercase;
    color: var(--ink-soft);
  }
  .rate-hint {
    font-size: 11px;
    line-height: 1.5;
    color: var(--ink-faint);
    white-space: pre-line;
  }
  .estimate {
    font-size: 12px;
    font-weight: 650;
    color: var(--gilt-deep);
  }

  /* output mode: one quiet segmented control */
  .segmented {
    display: inline-flex;
    gap: 3px;
    padding: 3px;
    background: var(--paper-dim);
    border: 1px solid var(--paper-edge);
    border-radius: 9px;
    align-self: flex-start;
  }
  .segmented button {
    padding: 7px 18px;
    border-radius: 6px;
    font-size: 12.5px;
    font-weight: 600;
    color: var(--ink-soft);
    white-space: nowrap;
    transition: color 0.12s ease, background 0.12s ease, box-shadow 0.12s ease;
  }
  .segmented button.on {
    background: #fffdf7;
    color: var(--ink);
    box-shadow: 0 0 0 1px var(--gilt), 0 2px 6px rgba(199, 154, 59, 0.16);
  }

  /* action bar */
  .actions {
    display: flex;
    align-items: center;
    gap: 14px;
    border-top: 1px solid var(--paper-edge);
    padding-top: 16px;
    margin-top: 20px;
  }
  .btn-group {
    display: flex;
    gap: 10px;
    margin-left: auto;
  }
  .hint {
    font-size: 12.5px;
    color: var(--ink-soft);
  }
  .linkish {
    color: var(--gilt-deep);
    text-decoration: underline;
    font-size: 12px;
  }
  .error {
    margin-top: 12px;
    color: var(--error);
    font-size: 13px;
  }

  @media (max-width: 620px) {
    .split {
      grid-template-columns: 1fr;
    }
    .cover {
      max-width: 170px;
    }
  }
</style>
