import {
  getSettings,
  onJobLog,
  onJobProgress,
  type BookInfo,
  type JobProgress,
  type Settings,
} from "$lib/api";

export type View = "drop" | "ready" | "running";

export const app = $state({
  view: "drop" as View,
  loading: false,
  error: "" as string,
  book: null as BookInfo | null,
  progress: null as JobProgress | null,
  logs: [] as string[],
  settingsOpen: false,
  settings: null as Settings | null,
  savedPath: "" as string,
});

export async function init() {
  try {
    app.settings = await getSettings();
  } catch (e) {
    app.error = String(e);
  }
  await onJobProgress((p) => {
    app.progress = p;
    if (p.status === "running" && app.view !== "running") app.view = "running";
  });
  await onJobLog((msg) => {
    app.logs = [...app.logs.slice(-40), msg];
  });
}

export function resetBook() {
  app.view = "drop";
  app.book = null;
  app.progress = null;
  app.logs = [];
  app.savedPath = "";
  app.error = "";
}
