import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

// In a plain browser (pnpm dev without Tauri) fall back to fixture data so
// the UI can be developed and reviewed outside the desktop shell.
const inTauri = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
export const isDesktopApp = inTauri;

export interface Settings {
  apiKey: string;
  model: string;
  mode: string;
  customInstructions: string;
}

export interface SegmentInfo {
  id: string;
  title: string;
  blocks: number;
  chars: number;
}

export interface BookInfo {
  format: "epub" | "pdf";
  filePath: string;
  fileName: string;
  title: string;
  author: string;
  coverDataUrl: string | null;
  totalChars: number;
  segments: SegmentInfo[];
  warnings: string[];
}

export interface JobOptions {
  targetLang: string;
  mode: string;
  model: string;
  customInstructions: string;
}

export interface SegmentStatus {
  id: string;
  title: string;
  state: "pending" | "active" | "done" | "failed";
  done: number;
  total: number;
}

export interface JobProgress {
  status: "running" | "completed" | "partial" | "cancelled" | "failed";
  segments: SegmentStatus[];
  batchesTotal: number;
  batchesDone: number;
  batchesFailed: number;
  charsDone: number;
  charsTotal: number;
  tokensUsed: number;
  error: string | null;
  outputReady: boolean;
}

export interface TestKeyResult {
  reply: string;
  model: string;
  latencyMs: number;
}

export interface SaveResult {
  path: string;
  bytes: number;
}

export const getSettings = () =>
  inTauri
    ? invoke<Settings>("get_settings")
    : Promise.resolve({
        apiKey: "AIza-browser-preview-key",
        model: "gemini-3.5-flash-lite",
        mode: "translated",
        customInstructions: "",
      } as Settings);

export const saveSettings = (settings: Settings) =>
  inTauri ? invoke<void>("save_settings", { newSettings: settings }) : Promise.resolve();

export const testApiKey = (apiKey: string, model: string) =>
  inTauri
    ? invoke<TestKeyResult>("test_api_key", { apiKey, model })
    : Promise.reject(new Error("Browser preview cannot reach the Gemini API"));

const fixtureBook: BookInfo = {
  format: "epub",
  filePath: "/books/Garden.epub",
  fileName: "Garden.epub",
  title: "The Garden of Slow Things",
  author: "Ada Marsh",
  coverDataUrl: null,
  totalChars: 118_400,
  warnings: [],
  segments: [
    { id: "ch1", title: "Seeds", blocks: 62, chars: 19_400 },
    { id: "ch2", title: "Rain", blocks: 58, chars: 21_200 },
    { id: "ch3", title: "Rows", blocks: 74, chars: 26_100 },
    { id: "ch4", title: "Sprouts", blocks: 51, chars: 18_300 },
    { id: "ch5", title: "Frost", blocks: 63, chars: 33_400 },
  ],
};

export const inspectBook = (path: string) =>
  inTauri ? invoke<BookInfo>("inspect_book", { path }) : Promise.resolve(fixtureBook);

// ---------------------------------------------------------------------------
// Browser-preview job simulation. Unlike the real backend it is started per
// startJob() call, can be cancelled mid-flight, and emits Activity lines —
// so restart, pause and log paths are actually exercisable in `pnpm dev`.
// ---------------------------------------------------------------------------
type ProgressCb = (p: JobProgress) => void;
let previewProgressCb: ProgressCb | null = null;
let previewLogCb: ((msg: string) => void) | null = null;
let previewTimer: ReturnType<typeof setInterval> | null = null;
let previewTick = 0;

function previewStop() {
  if (previewTimer) {
    clearInterval(previewTimer);
    previewTimer = null;
  }
}

function previewSnap(doneSeg: number, i: number, status: JobProgress["status"]): JobProgress {
  return {
    status,
    segments: fixtureBook.segments.map((s, idx) => ({
      id: s.id,
      title: s.title,
      state:
        status === "completed"
          ? "done"
          : idx < doneSeg - 1
            ? "done"
            : idx === doneSeg - 1
              ? "active"
              : "pending",
      done:
        status === "completed"
          ? s.blocks
          : idx < doneSeg - 1
            ? s.blocks
            : idx === doneSeg - 1
              ? Math.floor(s.blocks / 2)
              : 0,
      total: s.blocks,
    })),
    batchesTotal: 48,
    batchesDone: status === "completed" ? 48 : Math.min(48, i * 3),
    batchesFailed: 0,
    charsDone: status === "completed" ? 118_400 : Math.min(118_400, i * 7_400),
    charsTotal: 118_400,
    tokensUsed: i * 2_300,
    error: null,
    outputReady: status === "completed",
  };
}

function previewFire() {
  const i = previewTick++;
  if (i > 16) {
    previewProgressCb?.(previewSnap(5, i, "completed"));
    if (i > 17) previewStop();
  } else {
    previewProgressCb?.(previewSnap(Math.min(5, 1 + Math.floor(i / 3)), i, "running"));
    if (i % 4 === 0) previewLogCb?.(`Translated batches ${i * 3 + 1}–${i * 3 + 3}`);
  }
}

export const startJob = (options: JobOptions) => {
  if (inTauri) return invoke<void>("start_job", { options });
  previewStop();
  previewTick = 0;
  previewLogCb?.(`Translating into ${options.targetLang} with ${options.model} (preview)`);
  previewTimer = setInterval(previewFire, 400);
  return Promise.resolve();
};

export const cancelJob = () => {
  if (inTauri) return invoke<void>("cancel_job");
  previewStop();
  previewProgressCb?.(previewSnap(Math.min(5, 1 + Math.floor(previewTick / 3)), previewTick, "cancelled"));
  return Promise.resolve();
};

export const saveOutput = (path: string) =>
  inTauri
    ? invoke<SaveResult>("save_output", { path })
    : Promise.resolve({ path, bytes: 123_456 } as SaveResult);

export const getCurrentBook = (): Promise<BookInfo | null> =>
  inTauri ? invoke<BookInfo | null>("get_current_book") : Promise.resolve(null);

export const getJobProgress = (): Promise<JobProgress | null> =>
  inTauri ? invoke<JobProgress | null>("get_job_progress") : Promise.resolve(null);

export const estimateRequests = (model: string): Promise<number | null> => {
  if (inTauri) return invoke<number | null>("estimate_requests", { model });
  // Browser preview: approximate with the calibrated token estimator.
  const budget = model.includes("lite") ? 5_000 : 10_000;
  const tokens = Math.ceil(fixtureBook.totalChars / 2.3);
  return Promise.resolve(Math.max(1, Math.ceil(tokens / budget)));
};

export function onJobProgress(cb: ProgressCb): Promise<UnlistenFn> {
  if (inTauri) return listen<JobProgress>("job-progress", (e) => cb(e.payload));
  previewProgressCb = cb;
  return Promise.resolve(() => {
    previewProgressCb = null;
  });
}

export function onJobLog(cb: (msg: string) => void): Promise<UnlistenFn> {
  if (inTauri) return listen<{ message: string }>("job-log", (e) => cb(e.payload.message));
  previewLogCb = cb;
  return Promise.resolve(() => {
    previewLogCb = null;
  });
}

export const LANGUAGES = ["Burmese (မြန်မာ)", "English"];

export const MODELS = [
  { id: "gemini-3.5-flash-lite", label: "Gemini 3.5 Flash-Lite" },
  { id: "gemini-3.5-flash", label: "Gemini 3.5 Flash" },
  { id: "gemini-3.6-flash", label: "Gemini 3.6 Flash" },
  { id: "gemini-3.7-flash", label: "Gemini 3.7 Flash" },
  { id: "gemini-3.8-flash", label: "Gemini 3.8 Flash" },
];

/// Free-tier rate limits shown under the model picker.
export const MODEL_RATE_HINT = "20 requests for flash, 500 requests for flash lite.";

export function formatChars(n: number): string {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M chars`;
  if (n >= 1_000) return `${Math.round(n / 1_000)}k chars`;
  return `${n} chars`;
}
