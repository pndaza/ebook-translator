<script lang="ts">
  import { saveSettings, testApiKey, MODELS, MODEL_RATE_HINT, isDesktopApp } from "$lib/api";
  import { app } from "$lib/stores.svelte";
  import UpdaterSection from "$lib/components/UpdaterSection.svelte";

  let apiKey = $state("");
  let model = $state("");
  let instructions = $state("");
  let testing = $state(false);
  let testResult = $state<{ ok: boolean; text: string } | null>(null);
  let savingState = $state(false);
  let modalEl: HTMLDivElement | null = $state(null);

  $effect(() => {
    if (app.settingsOpen && app.settings) {
      apiKey = app.settings.apiKey;
      model = app.settings.model;
      instructions = app.settings.customInstructions;
      testResult = null;
    }
  });

  // Move focus into the dialog when it opens.
  $effect(() => {
    if (app.settingsOpen) modalEl?.focus();
  });

  function close() {
    app.settingsOpen = false;
  }

  function onKeydown(e: KeyboardEvent) {
    if (e.key === "Escape") {
      e.preventDefault();
      close();
      return;
    }
    // Keep Tab cycling inside the dialog while it is open.
    if (e.key !== "Tab" || !modalEl) return;
    const focusables = [
      ...modalEl.querySelectorAll<HTMLElement>("button, input, textarea, select, a[href]"),
    ].filter((el) => !el.hasAttribute("disabled"));
    if (focusables.length === 0) return;
    const first = focusables[0];
    const last = focusables[focusables.length - 1];
    const active = document.activeElement;
    if (e.shiftKey && (active === first || !modalEl.contains(active))) {
      e.preventDefault();
      last.focus();
    } else if (!e.shiftKey && (active === last || !modalEl.contains(active))) {
      e.preventDefault();
      first.focus();
    }
  }

  async function test() {
    testing = true;
    testResult = null;
    try {
      const res = await testApiKey(apiKey, model);
      testResult = { ok: true, text: `${res.model} replied “${res.reply}” in ${res.latencyMs} ms` };
    } catch (e) {
      testResult = { ok: false, text: String(e) };
    } finally {
      testing = false;
    }
  }

  async function saveAll() {
    savingState = true;
    try {
      await saveSettings({
        apiKey: apiKey.trim(),
        model,
        mode: app.form?.mode ?? "translated",
        customInstructions: instructions,
      });
      app.settings = {
        apiKey: apiKey.trim(),
        model,
        mode: app.form?.mode ?? "translated",
        customInstructions: instructions,
      };
      close();
    } catch (e) {
      testResult = { ok: false, text: String(e) };
    } finally {
      savingState = false;
    }
  }
</script>

<svelte:window onkeydown={onKeydown} />
{#if app.settingsOpen}
  <div class="overlay" role="presentation"><button class="overlay-close" aria-label="Close settings" onclick={close}></button>
    <div
      class="modal"
      role="dialog"
      aria-modal="true"
      aria-label="Settings"
      tabindex="-1"
      bind:this={modalEl}>
      <header>
        <h2>Settings</h2>
        <button class="close" onclick={close} aria-label="Close settings">✕</button>
      </header>

      <div class="field">
        <label for="key">Google AI Studio API key</label>
        <input id="key" type="password" bind:value={apiKey} placeholder="AIza…" autocomplete="off" />
        <p class="note">
          From <span class="mono">aistudio.google.com/apikey</span>. Stored only in this app's
          settings on your machine. Book text is sent to Google with storage disabled
          (<span class="mono">store: false</span>).
        </p>
      </div>

      <div class="field">
        <label for="model">Default model</label>
        <div class="model-row">
          <select id="model" bind:value={model}>
            {#each MODELS as m}<option value={m.id}>{m.label}</option>{/each}
          </select>
          <button class="btn btn-ghost test" onclick={test} disabled={testing || !apiKey.trim()}>
            {testing ? "Testing…" : "Test key"}
          </button>
        </div>
        <p class="rate-hint">{MODEL_RATE_HINT}</p>
      </div>
      {#if testResult}
        <p class="result" class:bad={!testResult.ok} role="status">{testResult.text}</p>
      {/if}

      <div class="field">
        <label for="instr">Translator instructions <span class="opt">optional</span></label>
        <textarea
          id="instr"
          rows="3"
          bind:value={instructions}
          placeholder="e.g. Use formal register. Keep Pāli terms like “nibbāna” untranslated."></textarea>
        <p class="note">Appended to every request — glossary rules, tone, terms to keep as-is.</p>
      </div>

      {#if isDesktopApp}
        <UpdaterSection />
      {/if}

      <footer>
        <button class="btn btn-primary" onclick={saveAll} disabled={savingState}>
          {savingState ? "Saving…" : "Save settings"}
        </button>
      </footer>
    </div>
  </div>
{/if}

<style>
  .overlay {
    position: fixed;
    inset: 0;
    background: rgba(10, 16, 22, 0.62);
    display: grid;
    place-items: center;
    z-index: 50;
  }
  .overlay-close {
    position: absolute;
    inset: 0;
    width: 100%;
    cursor: default;
  }
  .modal {
    position: relative;
    outline: none;
  }
  .modal {
    width: min(480px, 92vw);
    background: var(--paper);
    border-radius: var(--radius);
    box-shadow: 0 24px 60px rgba(4, 8, 12, 0.55);
    padding: 22px 24px 18px;
    display: flex;
    flex-direction: column;
    gap: 13px;
    max-height: 88vh;
    overflow-y: auto;
  }
  header {
    display: flex;
    justify-content: space-between;
    align-items: center;
  }
  h2 {
    font-family: var(--font-display);
    font-size: 20px;
    font-weight: 560;
  }
  .close {
    color: var(--ink-faint);
    font-size: 14px;
    padding: 4px 8px;
    border-radius: 6px;
  }
  .close:hover {
    color: var(--ink);
    background: var(--paper-dim);
  }
  .model-row {
    display: flex;
    gap: 12px;
    align-items: center;
  }
  .model-row select {
    flex: 1;
  }
  .test {
    white-space: nowrap;
  }
  textarea {
    background: #fffdf7;
    border: 1px solid var(--paper-edge);
    border-radius: 8px;
    padding: 8px 12px;
    font: inherit;
    resize: vertical;
  }
  textarea:focus {
    border-color: var(--gilt);
    outline: none;
  }
  .note {
    font-size: 11.5px;
    color: var(--ink-soft);
  }
  .mono {
    font-family: var(--font-mono);
    font-size: 10.5px;
  }
  .rate-hint {
    font-size: 11px;
    line-height: 1.5;
    color: var(--ink-soft);
    white-space: pre-line;
  }
  .opt {
    font-weight: 400;
    text-transform: none;
    letter-spacing: 0;
    color: var(--ink-faint);
  }
  .result {
    font-size: 12.5px;
    color: var(--ok);
    background: #eef0e6;
    border-radius: 6px;
    padding: 7px 10px;
  }
  .result.bad {
    color: var(--error);
    background: #f5e7e2;
  }
  footer {
    display: flex;
    justify-content: flex-end;
    border-top: 1px solid var(--paper-edge);
    padding-top: 12px;
  }
</style>
