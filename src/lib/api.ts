import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

// In a plain browser (pnpm dev without Tauri) fall back to fixture data so
// the UI can be developed and reviewed outside the desktop shell.
const inTauri = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

export interface Settings {
  apiKey: string;
  model: string;
  targetLang: string;
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
  status: "running" | "completed" | "cancelled" | "failed";
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
        targetLang: "Burmese (မြန်မာ)",
        mode: "bilingual",
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

let previewJobStarted = false;

export const startJob = (options: JobOptions) => {
  if (inTauri) return invoke<void>("start_job", { options });
  previewJobStarted = true;
  return Promise.resolve();
};

export const cancelJob = () => (inTauri ? invoke<void>("cancel_job") : Promise.resolve());

export const saveOutput = (path: string) =>
  inTauri
    ? invoke<SaveResult>("save_output", { path })
    : Promise.resolve({ path, bytes: 123_456 } as SaveResult);

export function onJobProgress(cb: (p: JobProgress) => void): Promise<UnlistenFn> {
  if (inTauri) return listen<JobProgress>("job-progress", (e) => cb(e.payload));
  // Browser preview: emit a scripted sequence once start_job was invoked.
  let i = 0;
  const timer = setInterval(() => {
    if (!previewJobStarted) return;
    const doneSeg = Math.min(5, 1 + Math.floor(i / 3));
    cb({
      status: i > 16 ? "completed" : "running",
      segments: fixtureBook.segments.map((s, idx) => ({
        id: s.id,
        title: s.title,
        state: idx < doneSeg - 1 ? "done" : idx === doneSeg - 1 ? "active" : "pending",
        done:
          idx < doneSeg - 1
            ? s.blocks
            : idx === doneSeg - 1
              ? Math.floor(s.blocks / 2)
              : 0,
        total: s.blocks,
      })),
      batchesTotal: 48,
      batchesDone: Math.min(48, i * 3),
      batchesFailed: 0,
      charsDone: Math.min(118_400, i * 7_400),
      charsTotal: 118_400,
      tokensUsed: i * 2_300,
      error: null,
      outputReady: i > 16,
    });
    if (i > 17) clearInterval(timer);
    i++;
  }, 400);
  return Promise.resolve(() => clearInterval(timer));
}

export function onJobLog(cb: (msg: string) => void): Promise<UnlistenFn> {
  if (inTauri) return listen<{ message: string }>("job-log", (e) => cb(e.payload.message));
  return Promise.resolve(() => {});
}

export const LANGUAGES = [
  "Burmese (မြန်မာ)",
  "English",
  "Thai",
  "Chinese (Simplified)",
  "Chinese (Traditional)",
  "Japanese",
  "Korean",
  "Vietnamese",
  "Indonesian",
  "Hindi",
  "French",
  "German",
  "Spanish",
  "Portuguese",
  "Russian",
  "Arabic",
];

export const MODELS = [
  { id: "gemini-3.5-flash-lite", label: "Gemini 3.5 Flash-Lite" },
  { id: "gemini-3.5-flash", label: "Gemini 3.5 Flash" },
  { id: "gemini-3.6-flash", label: "Gemini 3.6 Flash" },
  { id: "gemini-3.7-flash", label: "Gemini 3.7 Flash" },
  { id: "gemini-3.8-flash", label: "Gemini 3.8 Flash" },
];

/// Free-tier rate limits shown under the model picker.
export const MODEL_RATE_HINT = "20 requests for flash.\n500 requests for flash lite.";

export function formatChars(n: number): string {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M chars`;
  if (n >= 1_000) return `${Math.round(n / 1_000)}k chars`;
  return `${n} chars`;
}
