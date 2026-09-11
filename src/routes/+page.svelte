<script lang="ts">
  import { onMount } from "svelte";
  import "@fontsource-variable/fraunces";
  import "$lib/app.css";
  import Header from "$lib/components/Header.svelte";
  import DropZone from "$lib/components/DropZone.svelte";
  import BookCard from "$lib/components/BookCard.svelte";
  import ProgressCard from "$lib/components/ProgressCard.svelte";
  import SettingsModal from "$lib/components/SettingsModal.svelte";
  import { app, init } from "$lib/stores.svelte";

  onMount(() => {
    void init();
  });
</script>

<div id="app">
  <Header />
  <main>
    {#if app.view === "drop" || !app.book}
      <DropZone onloaded={() => {}} />
    {:else if app.view === "ready"}
      <BookCard onstart={() => {}} />
    {:else if app.view === "running"}
      <ProgressCard />
      {#if !app.progress}
        <p class="starting">Starting the translation…</p>
      {/if}
    {/if}
  </main>
  {#if app.settingsOpen}
    <SettingsModal />
  {/if}
</div>

<style>
  main {
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    padding: 24px;
    overflow-y: auto;
  }
  .starting {
    margin-top: 14px;
    color: #8fa0ae;
    font-size: 13px;
  }
</style>
