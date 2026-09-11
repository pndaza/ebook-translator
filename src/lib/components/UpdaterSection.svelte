<script lang="ts">
  import { getVersion } from "@tauri-apps/api/app";
  import { check, type Update } from "@tauri-apps/plugin-updater";
  import { relaunch } from "@tauri-apps/plugin-process";

  type UpdateState =
    | "idle"
    | "checking"
    | "latest"
    | "available"
    | "downloading"
    | "restart"
    | "error";

  let phase = $state<UpdateState>("idle");
  let message = $state("");
  let currentVersion = $state("");
  let update: Update | null = null;

  $effect(() => {
    getVersion()
      .then((v) => (currentVersion = v))
      .catch(() => {});
  });

  async function checkForUpdates() {
    phase = "checking";
    message = "";
    try {
      update = await check();
      if (update) {
        phase = "available";
        message = `Version ${update.version} is available.`;
      } else {
        phase = "latest";
        message = `You're on the latest version${currentVersion ? ` (v${currentVersion})` : ""}.`;
      }
    } catch (e) {
      phase = "error";
      message = `Could not check for updates: ${String(e).replace(/^Error:\s*/, "")}`;
    }
  }

  async function install() {
    if (!update) return;
    phase = "downloading";
    message = "Downloading update…";
    try {
      await update.downloadAndInstall();
      phase = "restart";
      message = "Update downloaded — restart the app to install.";
    } catch (e) {
      phase = "error";
      message = `Download failed: ${String(e).replace(/^Error:\s*/, "")}`;
    }
  }
</script>

<div class="field">
  <span class="label">App updates</span>
  <div class="row">
    <button
      class="btn btn-ghost"
      onclick={checkForUpdates}
      disabled={phase === "checking" || phase === "downloading"}>
      {phase === "checking" ? "Checking…" : "Check for updates"}
    </button>
    {#if phase === "available"}
      <button class="btn btn-primary" onclick={install}>Download & install</button>
    {:else if phase === "restart"}
      <button class="btn btn-primary" onclick={() => relaunch()}>Restart to update</button>
    {/if}
  </div>
  {#if message}
    <p class="update-msg" class:error={phase === "error"} role="status">{message}</p>
  {/if}
</div>

<style>
  .label {
    font-size: 11px;
    font-weight: 700;
    letter-spacing: 0.09em;
    text-transform: uppercase;
    color: var(--ink-soft);
  }
  .row {
    display: flex;
    align-items: center;
    gap: 12px;
  }
  .update-msg {
    font-size: 12px;
    color: var(--ink-soft);
  }
  .update-msg.error {
    color: var(--error);
  }
</style>
