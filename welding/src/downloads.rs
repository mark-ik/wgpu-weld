//! Downloads: CEF offers one, `welding` picks where it lands, the host watches
//! and steers it.
//!
//! CEF asks `OnBeforeDownload` where to put the file and will cancel the
//! download unless the callback is answered *during* the call. There is no time
//! to ask the host and wait for a reply — on Linux and macOS the host thread is
//! CEF's UI thread, so waiting would stop the loop carrying the answer, the
//! same reason cookies are request-then-poll. So the destination is policy,
//! set once on `CefSurfaceConfig::download_dir`, and the host steers afterwards
//! with `cancel_download` / `pause_download` / `resume_download`.
//!
//! Those three cannot act immediately either: CEF's `DownloadItemCallback` is
//! callback-scoped, like the paint handles, and only exists inside
//! `OnDownloadUpdated`. A host request is therefore recorded and applied on the
//! next update, which arrives promptly while a download is running.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Identifies one download for as long as the producer lives.
pub type DownloadId = u32;

/// What the host asked to do with an in-flight download.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DownloadOp {
    Cancel,
    Pause,
    Resume,
}

/// Progress is reported at most this often per download, plus always once more
/// when it finishes. CEF updates far more often than any progress bar needs,
/// and an unthrottled event per update is a log flood waiting to happen.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Default)]
pub(crate) struct Downloads {
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    dir: Option<PathBuf>,
    pending: HashMap<DownloadId, DownloadOp>,
    last_progress: HashMap<DownloadId, Instant>,
    started: HashSet<DownloadId>,
}

impl Downloads {
    pub(crate) fn set_dir(&self, dir: Option<PathBuf>) {
        self.inner.lock().unwrap().dir = dir;
    }

    /// Where a download called `suggested_name` should be written, or `None`
    /// when no download directory is configured and the download is refused.
    ///
    /// The name is attacker-influenced: it arrives from the server's
    /// `Content-Disposition` or the URL. Only its final component is used, so a
    /// suggestion of `../../.bashrc` or `C:\Windows\evil.dll` lands inside the
    /// configured directory as `.bashrc` / `evil.dll` rather than escaping it.
    pub(crate) fn destination_for(&self, suggested_name: &str) -> Option<PathBuf> {
        let dir = self.inner.lock().unwrap().dir.clone()?;
        Some(native_path(dir.join(safe_file_name(suggested_name))))
    }

    pub(crate) fn is_enabled(&self) -> bool {
        self.inner.lock().unwrap().dir.is_some()
    }

    /// Record a host request. Applied on the download's next update.
    pub(crate) fn request(&self, id: DownloadId, op: DownloadOp) {
        self.inner.lock().unwrap().pending.insert(id, op);
    }

    pub(crate) fn take_pending(&self, id: DownloadId) -> Option<DownloadOp> {
        self.inner.lock().unwrap().pending.remove(&id)
    }

    /// True the first time this id is seen, so `DownloadStarted` is emitted
    /// once even though CEF keeps updating the same item.
    pub(crate) fn mark_started(&self, id: DownloadId) -> bool {
        self.inner.lock().unwrap().started.insert(id)
    }

    /// Whether `DownloadStarted` has already gone out for this id.
    ///
    /// CEF updates an item before it asks where to put it, so without this the
    /// host would hear about progress on a download it has not been told
    /// exists yet.
    pub(crate) fn has_started(&self, id: DownloadId) -> bool {
        self.inner.lock().unwrap().started.contains(&id)
    }

    /// True when this download is due another progress event.
    pub(crate) fn due_for_progress(&self, id: DownloadId, now: Instant) -> bool {
        let mut inner = self.inner.lock().unwrap();
        match inner.last_progress.get(&id) {
            Some(prev) if now.duration_since(*prev) < PROGRESS_INTERVAL => false,
            _ => {
                inner.last_progress.insert(id, now);
                true
            }
        }
    }

    /// Drop everything remembered about a finished or cancelled download.
    pub(crate) fn forget(&self, id: DownloadId) {
        let mut inner = self.inner.lock().unwrap();
        inner.pending.remove(&id);
        inner.last_progress.remove(&id);
        inner.started.remove(&id);
    }
}

/// The final path component of `name`, with anything that would leave the
/// download directory removed. Falls back to `download` when nothing usable is
/// left, so a hostile or empty suggestion still produces a writable path.
fn safe_file_name(name: &str) -> String {
    // Split on both separators: a Windows-style name can arrive on Linux and
    // `Path::file_name` would keep the backslashes as part of the name there.
    let last = name
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or_default()
        .trim()
        .trim_matches('.');
    let candidate = Path::new(last)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or_default();
    if candidate.is_empty() {
        "download".to_owned()
    } else {
        candidate.to_owned()
    }
}

/// Give CEF the platform's own separators.
///
/// CEF on Windows **silently discards** a download whose destination contains
/// forward slashes. `on_before_download` is answered, the transfer runs to
/// completion and reports every byte, and then nothing is written: no file, no
/// `.crdownload` partial, `is_complete` never true and `full_path` empty for
/// the life of the item. There is no error anywhere to notice.
///
/// A host is well within its rights to hand over `C:/downloads`, and one under
/// Git Bash or any POSIX-shaped config will. Normalising here costs nothing and
/// removes a failure mode whose only symptom is a file that never appears.
#[cfg(windows)]
fn native_path(path: PathBuf) -> PathBuf {
    let s = path.to_string_lossy();
    if s.contains('/') {
        PathBuf::from(s.replace('/', "\\"))
    } else {
        path
    }
}

#[cfg(not(windows))]
fn native_path(path: PathBuf) -> PathBuf {
    path
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_suggested_name_cannot_escape_the_download_directory() {
        let d = Downloads::default();
        d.set_dir(Some(PathBuf::from("/downloads")));
        for hostile in [
            "../../.bashrc",
            "../.bashrc",
            "/etc/passwd",
            "..\\..\\evil.dll",
            "C:\\Windows\\evil.dll",
        ] {
            let path = d.destination_for(hostile).expect("dir is set");
            assert_eq!(
                path.parent(),
                Some(Path::new("/downloads")),
                "{hostile} escaped to {path:?}"
            );
        }
    }

    #[test]
    fn an_unusable_name_still_produces_a_path() {
        let d = Downloads::default();
        d.set_dir(Some(PathBuf::from("/downloads")));
        for empty in ["", "   ", "..", "/", "\\"] {
            assert_eq!(
                d.destination_for(empty).unwrap(),
                Path::new("/downloads/download"),
                "{empty:?} produced no usable name"
            );
        }
    }

    #[test]
    fn an_ordinary_name_is_left_alone() {
        let d = Downloads::default();
        d.set_dir(Some(PathBuf::from("/downloads")));
        assert_eq!(
            d.destination_for("report.pdf").unwrap(),
            Path::new("/downloads/report.pdf")
        );
    }

    #[test]
    fn no_directory_means_no_destination() {
        let d = Downloads::default();
        assert!(!d.is_enabled());
        assert_eq!(d.destination_for("report.pdf"), None);
    }

    #[test]
    fn a_pending_op_is_delivered_once() {
        let d = Downloads::default();
        d.request(7, DownloadOp::Pause);
        assert_eq!(d.take_pending(7), Some(DownloadOp::Pause));
        assert_eq!(d.take_pending(7), None);
    }

    #[test]
    fn started_is_reported_once_per_download() {
        let d = Downloads::default();
        assert!(d.mark_started(1));
        assert!(!d.mark_started(1));
        assert!(d.mark_started(2));
    }

    #[test]
    fn nothing_is_reported_before_the_download_is_accepted() {
        let d = Downloads::default();
        assert!(!d.has_started(1), "CEF updates an item before offering it");
        d.mark_started(1);
        assert!(d.has_started(1));
        d.forget(1);
        assert!(!d.has_started(1));
    }

    #[test]
    fn progress_is_throttled_then_allowed_again() {
        let d = Downloads::default();
        let t0 = Instant::now();
        assert!(d.due_for_progress(1, t0), "first update always reports");
        assert!(!d.due_for_progress(1, t0 + Duration::from_millis(10)));
        assert!(d.due_for_progress(1, t0 + PROGRESS_INTERVAL));
        // Throttling is per download, not global.
        assert!(d.due_for_progress(2, t0 + Duration::from_millis(10)));
    }

    #[test]
    fn forgetting_a_download_clears_its_state() {
        let d = Downloads::default();
        d.request(3, DownloadOp::Cancel);
        d.mark_started(3);
        d.due_for_progress(3, Instant::now());
        d.forget(3);
        assert_eq!(d.take_pending(3), None);
        assert!(d.mark_started(3), "state survived forget()");
    }
}
