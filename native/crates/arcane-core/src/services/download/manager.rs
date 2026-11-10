use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use serde::Serialize;
use sha1::{Digest as Sha1Digest, Sha1};
use sha2::{Sha256, Sha512};
use uuid::Uuid;
use tokio::io::AsyncWriteExt;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

use crate::error::{AppError, AppResult};

use crate::events::{Event, EventName, EventSink};
use crate::models::download::{DownloadProgressEvent, DownloadStatus, DownloadTask};
use crate::paths::AppPaths;
use crate::services::network::connectivity::prefer_ipv4;

const MAX_ATTEMPTS: u32 = 3;
/// Files downloading at the same time across the WHOLE launcher (client
/// jar + libraries + assets + Java + mods all share it), so running those
/// phases in parallel can't pile up hundreds of connections. Thousands of
/// small assets are latency-bound, not bandwidth-bound; 48 in flight keeps
/// even a slow line saturated while each one waits on its round trip.
pub const MAX_PARALLEL_DOWNLOADS: usize = 48;
/// A connection that can't even be opened in this long is dead; retrying
/// it beats waiting on the OS's own TCP timeout. Not lower: real connects
/// to Mojang's Azure-hosted piston-data have been measured at ~10s on
/// flaky routes and still succeeded.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// Max silence between two reads of a response body. Without this, a single
/// stalled connection held its concurrency slot forever and a whole install
/// could sit at "99%" — the retry loop never fired because no error ever
/// arrived. Deliberately a per-read timeout, not a total one, so a large
/// file on a slow-but-alive connection is never cut off.
const READ_TIMEOUT: Duration = Duration::from_secs(20);
/// How often progress is pushed to the UI. One event per file (or per
/// chunk) turned into thousands of IPC messages and React re-renders
/// during an asset download, which slowed the whole app down with it.
const EMIT_INTERVAL: Duration = Duration::from_millis(250);
const WRITE_BUFFER: usize = 256 * 1024;

/// Shared state of one entry in the downloads list. A bulk download (every
/// asset or library of a version) is ONE of these, not one per file.
struct Control {
    cancel: CancellationToken,
    paused: AtomicBool,
    downloaded: AtomicU64,
    total: AtomicU64,
    speed: AtomicU64,
    /// Set (under the lock) once the final Completed/Failed event is sent,
    /// so a progress tick can never land after it and leave the UI stuck.
    finished: std::sync::Mutex<bool>,
}

struct RunningTask {
    control: Arc<Control>,
    name: String,
    instance_id: Option<String>,
}

#[derive(Clone)]
pub struct DownloadManager {
    sink: Arc<dyn EventSink>,
    client: reqwest::Client,
    paths: AppPaths,
    tasks: Arc<RwLock<HashMap<String, RunningTask>>>,
    slots: Arc<tokio::sync::Semaphore>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct ExpectedHash {
    pub sha1: Option<String>,
    pub sha256: Option<String>,
    pub sha512: Option<String>,
}

/// One file of a [`DownloadManager::download_many`] batch. `size` comes from
/// the version/asset metadata when known, so the batch can show a real
/// percentage from the very first byte instead of guessing.
pub struct DownloadItem {
    pub url: String,
    pub dest: PathBuf,
    pub expected: ExpectedHash,
    pub size: Option<u64>,
}

impl DownloadManager {
    pub fn new(sink: Arc<dyn EventSink>, paths: AppPaths) -> Self {
        let client = prefer_ipv4(
            reqwest::Client::builder()
                .user_agent("ArcaneLauncher/0.1 (+mod-manager)")
                .connect_timeout(CONNECT_TIMEOUT)
                .read_timeout(READ_TIMEOUT)
                .pool_max_idle_per_host(MAX_PARALLEL_DOWNLOADS)
                .tcp_nodelay(true),
        )
        .build()
        .expect("http client");
        Self {
            sink,
            client,
            paths,
            tasks: Arc::new(RwLock::new(HashMap::new())),
            slots: Arc::new(tokio::sync::Semaphore::new(MAX_PARALLEL_DOWNLOADS)),
        }
    }

    pub async fn list(&self) -> Vec<DownloadTask> {
        let tasks = self.tasks.read().await;
        tasks
            .iter()
            .map(|(id, t)| DownloadTask {
                id: id.clone(),
                name: t.name.clone(),
                instance_id: t.instance_id.clone(),
                url: String::new(),
                downloaded_bytes: t.control.downloaded.load(Ordering::Relaxed),
                total_bytes: t.control.total.load(Ordering::Relaxed),
                speed_bps: t.control.speed.load(Ordering::Relaxed),
                status: if t.control.paused.load(Ordering::Relaxed) {
                    DownloadStatus::Paused
                } else {
                    DownloadStatus::Downloading
                },
                error: None,
            })
            .collect()
    }

    pub async fn pause(&self, task_id: &str) {
        if let Some(task) = self.tasks.read().await.get(task_id) {
            task.control.paused.store(true, Ordering::Relaxed);
        }
    }

    pub async fn resume(&self, task_id: &str) {
        if let Some(task) = self.tasks.read().await.get(task_id) {
            task.control.paused.store(false, Ordering::Relaxed);
        }
    }

    pub async fn cancel(&self, task_id: &str) {
        if let Some(task) = self.tasks.read().await.get(task_id) {
            task.control.cancel.cancel();
        }
    }

    /// Downloads `url` into a temp file, verifies it against any provided
    /// hash, and only then moves it into `final_path`. On any failure the
    /// temp file is removed and `final_path` is never touched, so a
    /// half-downloaded file can never end up in an instance's mods folder.
    ///
    /// If a matching file already exists in the shared download cache
    /// (keyed by sha1), it is reused instead of hitting the network again.
    pub async fn download_file(
        &self,
        name: &str,
        url: &str,
        final_path: PathBuf,
        expected: ExpectedHash,
        instance_id: Option<String>,
    ) -> AppResult<PathBuf> {
        let cache_path = expected.sha1.as_ref().map(|sha1| self.paths.downloads_cache_dir().join(sha1));
        if let (Some(cached), Some(sha1)) = (&cache_path, &expected.sha1) {
            if cached.exists() && verify_sha1(cached, sha1).await.unwrap_or(false) {
                copy_into_place(cached, &final_path).await?;
                return Ok(final_path);
            }
        }

        let (task_id, control) = self.register(name, instance_id).await;
        let result = self.fetch_with_retries(url, &final_path, &expected, &control, true).await;
        self.finish(&task_id, name, &control, result.as_ref().err()).await;
        result?;

        if let Some(cache_path) = cache_path {
            if let Some(parent) = cache_path.parent() {
                let _ = tokio::fs::create_dir_all(parent).await;
            }
            let _ = tokio::fs::copy(&final_path, &cache_path).await;
        }
        Ok(final_path)
    }

    /// Downloads many files at once, up to `concurrency` in flight, shown
    /// in the UI as ONE download named `name` with combined bytes/speed.
    /// A fresh Minecraft install needs 50-150+ libraries and thousands of
    /// small asset objects; that volume of tiny round trips — not bandwidth
    /// — is what makes a first launch slow, so they run concurrently and
    /// each file is written exactly once (hashed while streaming, no
    /// read-back, no extra copy into the download cache: assets and
    /// libraries already live in their own shared, content-addressed store).
    /// The first failure cancels the rest of the batch.
    pub async fn download_many(
        &self,
        name: &str,
        items: Vec<DownloadItem>,
        instance_id: Option<String>,
        concurrency: usize,
    ) -> AppResult<()> {
        if items.is_empty() {
            return Ok(());
        }
        let (task_id, control) = self.register(name, instance_id).await;
        let known_total: u64 = items.iter().filter_map(|i| i.size).sum();
        control.total.store(known_total, Ordering::Relaxed);

        let results: Vec<AppResult<()>> = futures_util::stream::iter(items)
            .map(|item| {
                let manager = self.clone();
                let control = control.clone();
                async move {
                    let result = manager
                        .fetch_with_retries(&item.url, &item.dest, &item.expected, &control, item.size.is_none())
                        .await;
                    if result.is_err() {
                        control.cancel.cancel();
                    }
                    result
                }
            })
            .buffer_unordered(concurrency.max(1))
            .collect()
            .await;

        // The first real error, not the "cancelled" ones it caused in the
        // files that were still in flight when it happened.
        let error = results
            .into_iter()
            .filter_map(Result::err)
            .min_by_key(|e| e.technical == CANCELLED_TECHNICAL);
        self.finish(&task_id, name, &control, error.as_ref()).await;
        match error {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    async fn register(&self, name: &str, instance_id: Option<String>) -> (String, Arc<Control>) {
        let task_id = Uuid::new_v4().to_string();
        let control = Arc::new(Control {
            cancel: CancellationToken::new(),
            paused: AtomicBool::new(false),
            downloaded: AtomicU64::new(0),
            total: AtomicU64::new(0),
            speed: AtomicU64::new(0),
            finished: std::sync::Mutex::new(false),
        });
        self.tasks.write().await.insert(
            task_id.clone(),
            RunningTask {
                control: control.clone(),
                name: name.to_string(),
                instance_id,
            },
        );
        self.spawn_progress_ticker(task_id.clone(), name.to_string(), control.clone());
        (task_id, control)
    }

    /// Emits throttled progress (with a real bytes/sec figure) for as long
    /// as the task is registered.
    fn spawn_progress_ticker(&self, task_id: String, name: String, control: Arc<Control>) {
        let manager = self.clone();
        tokio::spawn(async move {
            let mut last_bytes = 0u64;
            let mut last_tick = Instant::now();
            let mut smoothed = 0f64;
            loop {
                tokio::time::sleep(EMIT_INTERVAL).await;
                if !manager.tasks.read().await.contains_key(&task_id) {
                    break;
                }
                let now = control.downloaded.load(Ordering::Relaxed);
                let elapsed = last_tick.elapsed().as_secs_f64().max(0.001);
                let instant = now.saturating_sub(last_bytes) as f64 / elapsed;
                // Light smoothing so the number doesn't flicker between ticks.
                smoothed = if smoothed == 0.0 { instant } else { smoothed * 0.6 + instant * 0.4 };
                control.speed.store(smoothed as u64, Ordering::Relaxed);
                last_bytes = now;
                last_tick = Instant::now();
                let status = if control.paused.load(Ordering::Relaxed) {
                    DownloadStatus::Paused
                } else {
                    DownloadStatus::Downloading
                };
                let finished = control.finished.lock().unwrap_or_else(|e| e.into_inner());
                if *finished {
                    break;
                }
                manager.emit(&task_id, &name, &control, status, None);
            }
        });
    }

    async fn finish(&self, task_id: &str, name: &str, control: &Control, error: Option<&AppError>) {
        self.tasks.write().await.remove(task_id);
        control.speed.store(0, Ordering::Relaxed);
        let mut finished = control.finished.lock().unwrap_or_else(|e| e.into_inner());
        *finished = true;
        match error {
            None => {
                // A batch whose sizes were estimated still ends at exactly 100%.
                let done = control.downloaded.load(Ordering::Relaxed);
                control.total.fetch_max(done, Ordering::Relaxed);
                control.downloaded.fetch_max(control.total.load(Ordering::Relaxed), Ordering::Relaxed);
                self.emit(task_id, name, control, DownloadStatus::Completed, None);
            }
            Some(e) => {
                let status = if control.cancel.is_cancelled() && e.technical == CANCELLED_TECHNICAL {
                    DownloadStatus::Cancelled
                } else {
                    DownloadStatus::Failed
                };
                self.emit(task_id, name, control, status, Some(e.message.clone()));
            }
        }
    }

    /// `count_total`: add the response's Content-Length to the task total
    /// (for files whose size wasn't known up front).
    async fn fetch_with_retries(
        &self,
        url: &str,
        final_path: &Path,
        expected: &ExpectedHash,
        control: &Control,
        count_total: bool,
    ) -> AppResult<()> {
        let mut last_err: Option<AppError> = None;
        for attempt in 1..=MAX_ATTEMPTS {
            let mut written = 0u64;
            let mut counted_total = 0u64;
            let result = self
                .attempt_download(url, final_path, expected, control, count_total, &mut written, &mut counted_total)
                .await;
            match result {
                Ok(()) => return Ok(()),
                Err(e) => {
                    // Roll this attempt's bytes back out of the shared
                    // progress so a retry doesn't push a batch past 100%.
                    control.downloaded.fetch_sub(written, Ordering::Relaxed);
                    control.total.fetch_sub(counted_total, Ordering::Relaxed);
                    // A 404/403 won't fix itself; anything else (timeouts,
                    // dropped connections, a file briefly locked by an
                    // antivirus scan) is worth another try.
                    let permanent_http = !e.retryable && e.technical.starts_with("GET ");
                    if control.cancel.is_cancelled() || permanent_http {
                        return Err(e);
                    }
                    log::warn!("download attempt {attempt}/{MAX_ATTEMPTS} for {url} failed: {}", e.technical);
                    last_err = Some(e);
                    if attempt < MAX_ATTEMPTS {
                        tokio::select! {
                            _ = tokio::time::sleep(Duration::from_millis(500 * attempt as u64)) => {}
                            _ = control.cancel.cancelled() => return Err(cancelled()),
                        }
                    }
                }
            }
        }
        Err(last_err.unwrap_or_else(|| AppError::new("Download failed.", "unknown download error")))
    }

    #[allow(clippy::too_many_arguments)]
    async fn attempt_download(
        &self,
        url: &str,
        final_path: &Path,
        expected: &ExpectedHash,
        control: &Control,
        count_total: bool,
        written: &mut u64,
        counted_total: &mut u64,
    ) -> AppResult<()> {
        let _slot = tokio::select! {
            permit = self.slots.acquire() => permit.map_err(|_| cancelled())?,
            _ = control.cancel.cancelled() => return Err(cancelled()),
        };
        // The temp file sits next to its destination (same drive), so the
        // final move is always a cheap rename, never a cross-drive copy.
        let parent = final_path.parent().map(Path::to_path_buf).unwrap_or_else(|| self.paths.downloads_temp_dir());
        tokio::fs::create_dir_all(&parent).await?;
        let temp_path = parent.join(format!(".{}.part", Uuid::new_v4()));

        let result = self
            .stream_to(url, &temp_path, expected, control, count_total, written, counted_total)
            .await;
        if let Err(e) = result {
            let _ = tokio::fs::remove_file(&temp_path).await;
            return Err(e);
        }
        if let Err(e) = move_into_place(&temp_path, final_path).await {
            let _ = tokio::fs::remove_file(&temp_path).await;
            return Err(e);
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn stream_to(
        &self,
        url: &str,
        temp_path: &Path,
        expected: &ExpectedHash,
        control: &Control,
        count_total: bool,
        written: &mut u64,
        counted_total: &mut u64,
    ) -> AppResult<()> {
        let response = tokio::select! {
            r = self.client.get(url).send() => r?,
            _ = control.cancel.cancelled() => return Err(cancelled()),
        };
        if !response.status().is_success() {
            let status = response.status();
            let message = "The file could not be downloaded.";
            let technical = format!("GET {url} -> {status}");
            // 4xx (other than rate limits) won't fix itself on retry.
            return Err(if status.is_server_error() || status.as_u16() == 429 {
                AppError::retryable(message, technical)
            } else {
                AppError::new(message, technical)
            });
        }
        if count_total {
            if let Some(len) = response.content_length() {
                control.total.fetch_add(len, Ordering::Relaxed);
                *counted_total = len;
            }
        }

        let file = tokio::fs::File::create(temp_path).await?;
        let mut file = tokio::io::BufWriter::with_capacity(WRITE_BUFFER, file);
        let mut hasher = StreamHasher::for_expected(expected);
        let mut stream = response.bytes_stream();

        loop {
            while control.paused.load(Ordering::Relaxed) {
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_millis(200)) => {}
                    _ = control.cancel.cancelled() => return Err(cancelled()),
                }
            }
            let next = tokio::select! {
                n = stream.next() => n,
                _ = control.cancel.cancelled() => return Err(cancelled()),
            };
            match next {
                Some(Ok(chunk)) => {
                    hasher.update(&chunk);
                    file.write_all(&chunk).await?;
                    *written += chunk.len() as u64;
                    control.downloaded.fetch_add(chunk.len() as u64, Ordering::Relaxed);
                }
                Some(Err(e)) => {
                    return Err(AppError::retryable("The download connection dropped.", e.to_string()));
                }
                None => break,
            }
        }
        file.flush().await?;
        drop(file);

        if let Some((kind, want)) = hasher.finish_and_compare(expected) {
            return Err(AppError::retryable(
                "Download verification failed.",
                format!("{kind} mismatch for {url} (expected {want})"),
            ));
        }
        Ok(())
    }

    fn emit(&self, task_id: &str, name: &str, control: &Control, status: DownloadStatus, error: Option<String>) {
        let _ = self.sink.emit(
            Event::raw(EventName::DownloadProgress, DownloadProgressEvent {
                task_id: task_id.to_string(),
                name: name.to_string(),
                downloaded_bytes: control.downloaded.load(Ordering::Relaxed),
                total_bytes: control.total.load(Ordering::Relaxed),
                speed_bps: control.speed.load(Ordering::Relaxed),
                status,
                error,
            })
            .expect("DownloadProgressEvent serializes"),
        );
    }
}

const CANCELLED_TECHNICAL: &str = "cancelled by user";

fn cancelled() -> AppError {
    AppError::new("Download cancelled.", CANCELLED_TECHNICAL)
}

/// Hashes the body as it streams in, so verifying a download never needs
/// a second full read of the file from disk. Same priority as before:
/// sha1, then sha256, then sha512 — whichever the source provided.
enum StreamHasher {
    Sha1(Sha1),
    Sha256(Sha256),
    Sha512(Sha512),
    None,
}

impl StreamHasher {
    fn for_expected(expected: &ExpectedHash) -> Self {
        if expected.sha1.is_some() {
            Self::Sha1(Sha1::new())
        } else if expected.sha256.is_some() {
            Self::Sha256(Sha256::new())
        } else if expected.sha512.is_some() {
            Self::Sha512(Sha512::new())
        } else {
            Self::None
        }
    }

    fn update(&mut self, bytes: &[u8]) {
        match self {
            Self::Sha1(h) => h.update(bytes),
            Self::Sha256(h) => h.update(bytes),
            Self::Sha512(h) => h.update(bytes),
            Self::None => {}
        }
    }

    /// `None` when the hash matches (or there was nothing to check),
    /// otherwise the algorithm name and the expected value.
    fn finish_and_compare(self, expected: &ExpectedHash) -> Option<(&'static str, String)> {
        let (kind, actual, want) = match self {
            Self::Sha1(h) => ("sha1", hex::encode(h.finalize()), expected.sha1.clone()?),
            Self::Sha256(h) => ("sha256", hex::encode(h.finalize()), expected.sha256.clone()?),
            Self::Sha512(h) => ("sha512", hex::encode(h.finalize()), expected.sha512.clone()?),
            Self::None => return None,
        };
        if actual.eq_ignore_ascii_case(&want) {
            None
        } else {
            Some((kind, want))
        }
    }
}

async fn move_into_place(from: &Path, to: &Path) -> AppResult<()> {
    if let Some(parent) = to.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    // Windows' rename refuses to replace an existing file, so clear the
    // target first (e.g. re-downloading a corrupt library).
    if tokio::fs::rename(from, to).await.is_err() {
        let _ = tokio::fs::remove_file(to).await;
        if tokio::fs::rename(from, to).await.is_err() {
            tokio::fs::copy(from, to).await?;
            let _ = tokio::fs::remove_file(from).await;
        }
    }
    Ok(())
}

async fn copy_into_place(from: &Path, to: &Path) -> AppResult<()> {
    if let Some(parent) = to.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::copy(from, to).await?;
    Ok(())
}

pub async fn verify_sha1(path: &Path, expected: &str) -> std::io::Result<bool> {
    let bytes = tokio::fs::read(path).await?;
    let mut hasher = Sha1::new();
    hasher.update(&bytes);
    let actual = hex::encode(hasher.finalize());
    Ok(actual.eq_ignore_ascii_case(expected))
}

pub async fn verify_sha256(path: &Path, expected: &str) -> std::io::Result<bool> {
    let bytes = tokio::fs::read(path).await?;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    let actual = hex::encode(hasher.finalize());
    Ok(actual.eq_ignore_ascii_case(expected))
}

pub async fn verify_sha512(path: &Path, expected: &str) -> std::io::Result<bool> {
    let bytes = tokio::fs::read(path).await?;
    let mut hasher = Sha512::new();
    hasher.update(&bytes);
    let actual = hex::encode(hasher.finalize());
    Ok(actual.eq_ignore_ascii_case(expected))
}
