<script lang="ts">
  import { app } from "$lib/stores.svelte";

  let { onNewBook }: { onNewBook?: () => void } = $props();

  // A finished job keeps view "running"; only hide the button while work is in flight.
  const busy = $derived(
    app.view === "running" && (app.progress == null || app.progress.status === "running"),
  );
</script>

<header>
  <div class="wordmark">
    <span class="tick" aria-hidden="true"></span>
    <h1>Ebook Translator</h1>
  </div>
  <nav>
    {#if app.book && !busy}
      <button class="chrome-btn" onclick={() => onNewBook?.()}>Another book</button>
    {/if}
    <button
      class="chrome-btn"
      onclick={() => (app.settingsOpen = true)}
      aria-label="Settings"
      title="Settings">
      ⚙ Settings
    </button>
  </nav>
</header>

<style>
  header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 14px 22px 12px;
    border-bottom: 1px solid var(--cloth-line);
    background:
      radial-gradient(120% 180% at 50% -60%, #24374b 0%, var(--cloth) 55%, var(--cloth-deep) 100%);
  }
  .wordmark {
    display: flex;
    align-items: center;
    gap: 10px;
  }
  .wordmark h1 {
    font-family: var(--font-display);
    font-weight: 560;
    font-size: 19px;
    letter-spacing: 0.015em;
    color: #faf7ef;
  }
  .tick {
    width: 18px;
    height: 2px;
    background: var(--gilt);
    border-radius: 2px;
  }
  nav {
    display: flex;
    gap: 10px;
  }
  .chrome-btn {
    color: #b8c4cf;
    font-size: 12.5px;
    padding: 6px 12px;
    border-radius: 999px;
    border: 1px solid var(--cloth-line);
    transition: color 0.12s ease, border-color 0.12s ease;
  }
  .chrome-btn:hover {
    color: var(--gilt-bright);
    border-color: var(--gilt-deep);
  }
</style>
