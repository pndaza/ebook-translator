import {
  getCurrentBook,
  getJobProgress,
  getSettings,
  onJobLog,
  onJobProgress,
  type BookInfo,
  type JobOptions,
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
  // Translate-form choices, kept in the store so they survive leaving the
  // ready view (Back to book / settings save) instead of resetting to
  // defaults — which would silently break the language-keyed resume cache.
  form: null as null | { lang: string; mode: string; model: string },
  // Language the current job runs with, used for the output filename.
  jobLang: "" as string,
  // Options the current job was started with, so the sample footer can
  // launch the matching full run.
  lastOptions: null as JobOptions | null,
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
  // After a webview reload the backend may still hold a book and a job.
  try {
    const [book, progress] = await Promise.all([getCurrentBook(), getJobProgress()]);
    if (progress) {
      app.progress = progress;
      if (book) app.book = book;
      app.view = "running";
    } else if (book && !app.book) {
      app.book = book;
      app.view = "ready";
    }
  } catch {
    // Desktop-only restore; ignore in the browser preview.
  }
}

export function resetBook() {
  app.view = "drop";
  app.book = null;
  app.progress = null;
  app.logs = [];
  app.savedPath = "";
  app.error = "";
  app.form = null;
  app.lastOptions = null;
}
