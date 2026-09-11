use crate::settings::Settings;
use crate::types::{JobProgress, LoadedBook};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};

pub type BookRef = Arc<Mutex<LoadedBook>>;

pub struct AppState {
    pub settings: RwLock<Settings>,
    pub settings_path: PathBuf,
    pub data_dir: PathBuf,
    /// The currently loaded book, shared with the running job via Arc.
    pub book: Mutex<Option<BookRef>>,
    /// Assembled EPUB bytes once a job completes successfully.
    pub output: Arc<Mutex<Option<Vec<u8>>>>,
    pub job_cancel: Arc<AtomicBool>,
    pub job_running: Arc<AtomicBool>,
    /// Latest job-progress snapshot, so the UI can re-sync after a reload.
    pub progress: Arc<Mutex<Option<JobProgress>>>,
}

impl AppState {
    pub fn job_is_running(&self) -> bool {
        self.job_running.load(Ordering::SeqCst)
    }
}
