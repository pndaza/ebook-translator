<script lang="ts">
  import { getCurrentWebview } from "@tauri-apps/api/webview";
  import { open } from "@tauri-apps/plugin-dialog";
  import { app } from "$lib/stores.svelte";
  import { inspectBook } from "$lib/api";

  let { onloaded }: { onloaded: () => void } = $props();
  let dragging = $state(false);

  $effect(() => {
    // Browser preview has no Tauri webview; getCurrentWebview() would throw.
    if (typeof window === "undefined" || !("__TAURI_INTERNALS__" in window)) return;
    let disposed = false;
    let unlisten: (() => void) | null = null;
    const p = getCurrentWebview().onDragDropEvent((event) => {
      if (event.payload.type === "over") {
        dragging = true;
      } else if (event.payload.type === "leave") {
        dragging = false;
      } else if (event.payload.type === "drop") {
        dragging = false;
        void load(event.payload.paths[0]);
      }
    });
    // The listener resolves asynchronously — if the component is already
    // gone, dispose immediately instead of leaking a global handler.
    p.then((f) => {
      if (disposed) f();
      else unlisten = f;
    });
    return () => {
      disposed = true;
      unlisten?.();
    };
  });

  async function browse() {
    if (typeof window !== "undefined" && !("__TAURI_INTERNALS__" in window)) {
      // Browser preview: load the fixture book directly.
      await load("Garden.epub");
      return;
    }
    const path = await open({
      multiple: false,
      filters: [{ name: "Ebooks", extensions: ["epub", "pdf"] }],
    });
    if (typeof path === "string") await load(path);
  }

  async function load(path: string) {
    if (!path) return;
    app.loading = true;
    app.error = "";
    try {
      app.book = await inspectBook(path);
      app.view = "ready";
      app.progress = null;
      app.logs = [];
      app.savedPath = "";
      onloaded();
    } catch (e) {
      app.error = String(e);
    } finally {
      app.loading = false;
    }
  }
</script>

<section class="wrap">
  <div class="drop" class:dragging role="button" tabindex="0" aria-label="Choose an ebook file" onclick={browse} onkeydown={(e) => (e.key === "Enter" || e.key === " ") && (e.preventDefault(), browse())}>
    <div class="page left" aria-hidden="true"></div>
    <div class="page right" aria-hidden="true"></div>
    <div class="inner">
      {#if app.loading}
        <p class="title">Opening…</p>
      {:else}
        <p class="title">Drop a book here</p>
        <p class="sub">EPUB or PDF — or <span class="link">browse files</span></p>
        <p class="note">The translation runs on your Google AI Studio key, on your machine.</p>
      {/if}
    </div>
  </div>
  {#if app.error}
    <p class="error" role="alert">{app.error}</p>
  {/if}
</section>

<style>
  .wrap {
    flex: 1;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 18px;
    padding: 30px;
  }
  .drop {
    position: relative;
    width: min(460px, 90%);
    padding: 64px 40px;
    border: 1px dashed #46586c;
    border-radius: 14px;
    background: rgba(255, 255, 255, 0.02);
    text-align: center;
    transition: border-color 0.15s ease, background 0.15s ease, transform 0.15s ease;
  }
  .drop:hover,
  .drop.dragging {
    border-color: var(--gilt);
    background: rgba(199, 154, 59, 0.07);
  }
  .drop.dragging {
    transform: scale(1.015);
  }
  /* two facing pages behind the frame — the bilingual motif */
  .page {
    position: absolute;
    top: 14px;
    bottom: 14px;
    width: 34%;
    border: 1px solid #3a4c5f;
    border-radius: 4px;
    opacity: 0.35;
    pointer-events: none;
  }
  .page.left {
    left: 9%;
    transform: rotate(-4deg);
  }
  .page.right {
    right: 9%;
    transform: rotate(4deg);
  }
  .inner {
    position: relative;
    z-index: 1;
  }
  .title {
    font-family: var(--font-display);
    font-size: 26px;
    font-weight: 520;
    color: var(--paper);
    letter-spacing: 0.01em;
  }
  .sub {
    margin-top: 8px;
    color: #b8c4cf;
    font-size: 14px;
  }
  .link {
    color: var(--gilt-bright);
  }
  .note {
    margin-top: 22px;
    color: #71828f;
    font-size: 11.5px;
    letter-spacing: 0.02em;
  }
  .error {
    max-width: 460px;
    color: #e8a794;
    font-size: 13px;
    text-align: center;
  }
</style>
