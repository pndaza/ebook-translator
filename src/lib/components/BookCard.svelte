<script lang="ts">
  import { startJob, formatChars, LANGUAGES, MODELS, MODEL_RATE_HINT } from "$lib/api";
  import { app } from "$lib/stores.svelte";

  let { onstart }: { onstart: () => void } = $props();

  let lang = $state("");
  let mode = $state("bilingual");
  let model = $state("gemini-3.5-flash-lite");
  let starting = $state(false);

  $effect(() => {
    if (app.settings) {
      lang = app.settings.targetLang || LANGUAGES[0];
      mode = app.settings.mode || "bilingual";
      model = app.settings.model || "gemini-3.5-flash-lite";
    }
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
      app.view = "running";
      app.savedPath = "";
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
    <div class="overview">
      {#if book.coverDataUrl}
        <img class="cover" src={book.coverDataUrl} alt="" />
      {:else}
        <div class="cover blank"><span>{book.format === "pdf" ? "PDF" : "EPUB"}</span></div>
      {/if}
      <div class="meta">
        <p class="kind">{book.format === "pdf" ? "PDF document" : "EPUB edition"}</p>
        <h2>{book.title}</h2>
        {#if book.author}<p class="author">{book.author}</p>{/if}
        <p class="stats">
          <span class="mono">{book.segments.length}</span>
          {book.segments.length === 1 ? "section" : "sections"} ·
          <span class="mono">{formatChars(book.totalChars)}</span> ·
          <span class="mono">{book.segments.reduce((n, s) => n + s.blocks, 0)}</span> paragraphs
        </p>
      </div>
    </div>

    <div class="form">
      <div class="row">
        <div class="field grow">
          <label for="lang">Translate into</label>
          <input id="lang" list="langs" type="text" bind:value={lang} placeholder="Language" />
          <datalist id="langs">
            {#each LANGUAGES as l}<option value={l}></option>{/each}
          </datalist>
        </div>
        <div class="field">
          <label for="model">Model</label>
          <select id="model" bind:value={model}>
            {#each MODELS as m}<option value={m.id}>{m.label}</option>{/each}
          </select>
          <p class="rate-hint">{MODEL_RATE_HINT}</p>
        </div>
      </div>

      <div class="field">
        <span class="label" id="output-label">Output</span>
        <div class="facing" role="radiogroup" aria-label="Output mode">
          <button
            type="button"
            class="face"
            class:on={mode === "bilingual"}
            role="radio"
            aria-checked={mode === "bilingual"}
            onclick={() => (mode = "bilingual")}>
            <span class="pg a">The quick fox</span>
            <span class="pg b">မြန်မာလို</span>
            <span class="face-name">Bilingual</span>
            <span class="face-sub">translation after each paragraph</span>
          </button>
          <button
            type="button"
            class="face"
            class:on={mode === "translated"}
            role="radio"
            aria-checked={mode === "translated"}
            onclick={() => (mode = "translated")}>
            <span class="pg a only">မြန်မာလို</span>
            <span class="face-name">Translation only</span>
            <span class="face-sub">replaces the original text</span>
          </button>
        </div>
      </div>

      <div class="actions">
        {#if noKey}
          <p class="hint">
            No API key yet — <button class="linkish" onclick={() => (app.settingsOpen = true)}>add your Google AI Studio key</button> to start.
          </p>
        {/if}
        <button class="btn btn-primary" disabled={noKey || starting || !lang.trim()} onclick={begin}>
          {starting ? "Starting…" : `Translate to ${lang.split(" (")[0]}`}
        </button>
      </div>
    </div>

    {#if app.error}
      <p class="error" role="alert">{app.error}</p>
    {/if}
  </article>
{/if}

<style>
  .card {
    width: min(680px, 100%);
    background: var(--paper);
    border-radius: var(--radius);
    box-shadow: 0 14px 40px rgba(6, 12, 18, 0.45);
    padding: 26px 28px 22px;
  }
  .overview {
    display: flex;
    gap: 18px;
    align-items: flex-start;
    padding-bottom: 18px;
    border-bottom: 1px solid var(--paper-edge);
  }
  .cover {
    width: 92px;
    min-height: 130px;
    object-fit: cover;
    border-radius: 4px;
    box-shadow: 2px 3px 8px rgba(30, 25, 15, 0.3), 1px 0 0 rgba(0, 0, 0, 0.12) inset;
    background: var(--paper-dim);
  }
  .cover.blank {
    display: grid;
    place-items: center;
    background: #efe8d9;
    border-color: #d8ccb2;
    font-family: var(--font-mono);
    font-size: 11px;
    color: var(--ink-faint);
    border: 1px solid var(--paper-edge);
    box-shadow: none;
  }
  .kind {
    font-size: 10.5px;
    font-weight: 700;
    letter-spacing: 0.14em;
    text-transform: uppercase;
    color: var(--gilt-deep);
  }
  h2 {
    font-family: var(--font-display);
    font-size: 24px;
    font-weight: 560;
    line-height: 1.2;
    margin: 2px 0 2px;
  }
  .author {
    color: var(--ink-soft);
    font-size: 13.5px;
  }
  .stats {
    margin-top: 10px;
    color: var(--ink-soft);
    font-size: 12.5px;
  }
  .mono {
    font-family: var(--font-mono);
    font-size: 11.5px;
    color: var(--ink);
    background: var(--paper-dim);
    padding: 1px 5px;
    border-radius: 4px;
  }
  .form {
    padding-top: 18px;
    display: flex;
    flex-direction: column;
    gap: 16px;
  }
  .row {
    display: flex;
    gap: 14px;
    align-items: flex-start;
  }
  .grow {
    flex: 1;
  }
  .rate-hint {
    font-size: 11px;
    line-height: 1.5;
    color: var(--ink-soft);
    white-space: pre-line;
  }
  .row :global(select) {
    max-width: 250px;
    width: 100%;
    min-width: 0;
  }
  .row .field.grow {
    min-width: 0;
  }
  .field select {
    width: 100%;
  }
  .label {
    font-size: 11px;
    font-weight: 700;
    letter-spacing: 0.09em;
    text-transform: uppercase;
    color: var(--ink-soft);
  }

  /* facing-pages output selector — the signature */
  .facing {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 12px;
  }
  .face {
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 7px;
    padding: 14px 10px 12px;
    min-height: 132px;
    border: 1.5px solid #d5c9ad;
    border-radius: 8px;
    background: #fffdf7;
    transition: border-color 0.12s ease, box-shadow 0.12s ease;
  }
  .face:hover {
    border-color: var(--ink-faint);
  }
  .face.on {
    border-color: var(--gilt);
    box-shadow: 0 0 0 1px var(--gilt), 0 4px 14px rgba(199, 154, 59, 0.18);
  }
  .pg {
    font-size: 12.5px;
    line-height: 2;
    padding: 6px 16px;
    background: var(--paper-dim);
    border-radius: 3px 3px 0 0;
    box-shadow: 0 -1px 0 var(--paper-edge) inset;
    width: 82%;
    text-align: center;
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
  }
  .pg.b {
    color: var(--ink-soft);
    margin-top: 1px;
    border-radius: 0 0 3px 3px;
  }
  .pg.a.only {
    border-radius: 3px;
  }
  .face-name {
    margin-top: 4px;
    font-weight: 650;
    font-size: 13px;
  }
  .face-sub {
    font-size: 11px;
    color: var(--ink-faint);
  }

  .actions {
    display: flex;
    align-items: center;
    gap: 12px;
    min-height: 38px;
  }
  .linkish {
    color: var(--gilt-deep);
    text-decoration: underline;
    font-size: 12px;
  }
  .error {
    margin-top: 14px;
    color: var(--error);
    font-size: 13px;
  }
</style>
