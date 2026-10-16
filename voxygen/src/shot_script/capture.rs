//! Named screenshots with a completion signal.
//!
//! The F4 screenshot (`Window::take_screenshot`) picks its own file name and
//! reports through the HUD message channel; it is left untouched. A shot
//! script instead needs to choose the file name and to know when the PNG is on
//! disk, so `Window::take_screenshot_to` builds a [`ShotJob`] here and hands
//! [`ShotJob::complete`] to the renderer's GPU read-back as its callback.
//!
//! Everything except that one call is plain file and channel logic, so it is
//! unit-tested without a GPU.
//!
//! The completion channel must be able to hold the result without a waiting
//! receiver (unbounded, or bounded with free capacity): the result is sent
//! with `try_send` from the renderer's screenshot thread, which never blocks
//! on it. A full or zero-capacity channel loses the result (logged).

use crossbeam_channel::{Sender, TrySendError};
use std::{
    fmt,
    io::Write,
    path::{Component, Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

/// What the completion signal carries: the written file, or why it failed.
pub type ShotResult = Result<PathBuf, ShotError>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShotError {
    /// The target path was refused before anything was captured.
    InvalidPath { path: PathBuf, reason: &'static str },
    /// The GPU read-back of the frame failed.
    Readback(String),
    /// Encoding or writing the PNG failed.
    Write { path: PathBuf, error: String },
    /// The capture request was discarded before it ran (for example another
    /// screenshot request replaced it within the same frame).
    Dropped { path: PathBuf },
}

impl fmt::Display for ShotError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ShotError::InvalidPath { path, reason } => {
                write!(f, "refused screenshot path {}: {reason}", path.display())
            },
            ShotError::Readback(e) => write!(f, "GPU read-back failed: {e}"),
            ShotError::Write { path, error } => {
                write!(f, "couldn't write screenshot {}: {error}", path.display())
            },
            ShotError::Dropped { path } => {
                write!(f, "screenshot request for {} was discarded", path.display())
            },
        }
    }
}

impl std::error::Error for ShotError {}

impl ShotError {
    /// The error as it may appear in `report.json`: only file names, never
    /// the absolute paths that [`Display`](fmt::Display) (for the log) shows.
    pub fn report_reason(&self) -> String {
        let name = |path: &Path| {
            path.file_name()
                .map_or_else(|| "<unnamed>".into(), |n| n.to_string_lossy().into_owned())
        };
        match self {
            ShotError::InvalidPath { path, reason } => {
                format!("refused screenshot path {}: {reason}", name(path))
            },
            ShotError::Readback(e) => format!("GPU read-back failed: {e}"),
            ShotError::Write { path, error } => {
                // The underlying error text may repeat the full path.
                let file = name(path);
                let error = error.replace(&path.display().to_string(), &file);
                format!("couldn't write screenshot {file}: {error}")
            },
            ShotError::Dropped { path } => {
                format!("screenshot request for {} was discarded", name(path))
            },
        }
    }
}

/// Basic sanity check of a screenshot target: the path must be absolute,
/// contain no `..` component, name a `.png` file, and its parent directory
/// must already exist (no directory is ever created here).
///
/// This is not a confinement check: symlinks in the parent directory are not
/// resolved, and there is no output directory to compare against. Keeping
/// paths inside the run's output directory is the script layer's job.
pub fn validate_target(path: &Path) -> Result<(), &'static str> {
    if path.as_os_str().is_empty() {
        return Err("path is empty");
    }
    if !path.is_absolute() {
        return Err("path is not absolute");
    }
    if path.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err("path contains '..'");
    }
    match path.extension().and_then(|e| e.to_str()) {
        Some(ext) if ext.eq_ignore_ascii_case("png") => {},
        _ => return Err("file name does not end in .png"),
    }
    match path.parent() {
        Some(parent) if parent.is_dir() => Ok(()),
        _ => Err("parent directory does not exist"),
    }
}

/// Sends exactly one [`ShotResult`]: either the explicit one, or
/// [`ShotError::Dropped`] if it is dropped unsent.
struct DoneSignal {
    path: PathBuf,
    tx: Option<Sender<ShotResult>>,
}

/// Non-blocking send. A disconnected receiver (driver shut down) is fine; a
/// full channel is a caller bug and only logged, never waited on.
fn send_nonblocking(tx: &Sender<ShotResult>, result: ShotResult) {
    if let Err(TrySendError::Full(result)) = tx.try_send(result) {
        tracing::warn!(
            ?result,
            "shot-script screenshot result lost: completion channel is full"
        );
    }
}

impl DoneSignal {
    fn send(mut self, result: ShotResult) {
        if let Some(tx) = self.tx.take() {
            send_nonblocking(&tx, result);
        }
    }
}

impl Drop for DoneSignal {
    fn drop(&mut self) {
        if let Some(tx) = self.tx.take() {
            send_nonblocking(
                &tx,
                Err(ShotError::Dropped {
                    path: std::mem::take(&mut self.path),
                }),
            );
        }
    }
}

/// A validated screenshot request waiting for the frame read-back.
pub struct ShotJob {
    done: DoneSignal,
}

impl ShotJob {
    /// Validate `path`. On refusal the error is sent on `done` right away and
    /// `None` is returned, so the caller never queues a capture.
    pub fn prepare(path: PathBuf, done: Sender<ShotResult>) -> Option<Self> {
        let done = DoneSignal {
            path: path.clone(),
            tx: Some(done),
        };
        match validate_target(&path) {
            Ok(()) => Some(Self { done }),
            Err(reason) => {
                done.send(Err(ShotError::InvalidPath { path, reason }));
                None
            },
        }
    }

    pub fn path(&self) -> &Path { &self.done.path }

    /// The renderer callback: write the frame and signal the outcome. Runs on
    /// the renderer's screenshot thread; never panics.
    pub fn complete(self, image: Result<image::RgbImage, String>) {
        let result = match image {
            Ok(image) => write_png(&image, self.path()),
            Err(e) => {
                tracing::warn!(?e, "Couldn't generate shot-script screenshot");
                Err(ShotError::Readback(e))
            },
        };
        self.done.send(result);
    }
}

/// Per-process counter that makes every temporary file name unique.
static PARTIAL_COUNTER: AtomicU64 = AtomicU64::new(0);

/// A temporary name next to `path` that no other job (in this or another
/// process) uses: `<path>.<pid>.<n>.partial`.
fn partial_path(path: &Path) -> PathBuf {
    let n = PARTIAL_COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut partial = path.as_os_str().to_owned();
    partial.push(format!(".{}.{n}.partial", std::process::id()));
    PathBuf::from(partial)
}

/// Encode `image` as PNG into a fresh temporary file next to `path` and
/// rename it into place, so a reader never sees a half-written file at
/// `path`. An existing file at `path` is replaced.
///
/// The temporary file is opened with `create_new` (exclusive create), so a
/// pre-existing file or symlink at that name is never followed or reused; the
/// job fails with [`ShotError::Write`] instead. Leftover temporary files from
/// a crashed run are not touched: their names cannot collide with new ones,
/// and deleting files this job did not create is not its business.
fn write_png(image: &image::RgbImage, path: &Path) -> ShotResult {
    let write_err = |error: String| ShotError::Write {
        path: path.to_path_buf(),
        error,
    };

    let mut png = Vec::new();
    image
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .map_err(|e| write_err(format!("PNG encoding failed: {e}")))?;

    let partial = partial_path(path);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&partial)
        .map_err(|e| {
            tracing::warn!(?e, ?partial, "Couldn't create shot-script screenshot file");
            write_err(format!("couldn't create {}: {e}", partial.display()))
        })?;
    // From here on the temporary file is ours; remove it on any failure.
    let written = file.write_all(&png).and_then(|()| file.sync_all());
    drop(file);
    if let Err(e) = written.and_then(|()| std::fs::rename(&partial, path)) {
        let _ = std::fs::remove_file(&partial);
        tracing::warn!(?e, ?path, "Couldn't save shot-script screenshot");
        return Err(write_err(e.to_string()));
    }
    Ok(path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossbeam_channel::unbounded;

    /// A fresh, empty directory under the system temp dir.
    fn scratch_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "voxygen-shot-capture-{tag}-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        dir
    }

    fn test_image() -> image::RgbImage {
        image::RgbImage::from_fn(4, 3, |x, y| image::Rgb([x as u8 * 60, y as u8 * 80, 7]))
    }

    fn refused(path: &Path) -> &'static str {
        validate_target(path).expect_err("path should be refused")
    }

    #[test]
    fn validate_refuses_bad_paths() {
        let dir = scratch_dir("validate");
        assert_eq!(refused(Path::new("")), "path is empty");
        assert_eq!(refused(Path::new("shot.png")), "path is not absolute");
        assert_eq!(refused(Path::new("../shot.png")), "path is not absolute");
        assert_eq!(
            refused(&dir.join("..").join("shot.png")),
            "path contains '..'"
        );
        assert_eq!(
            refused(&dir.join("shot.jpg")),
            "file name does not end in .png"
        );
        assert_eq!(refused(&dir.join("shot")), "file name does not end in .png");
        assert_eq!(
            refused(&dir.join("missing").join("shot.png")),
            "parent directory does not exist"
        );
        assert_eq!(validate_target(&dir.join("shot.png")), Ok(()));
        assert_eq!(validate_target(&dir.join("shot.PNG")), Ok(()));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn invalid_path_signals_immediately_and_creates_nothing() {
        let dir = scratch_dir("invalid");
        let target = dir.join("sub").join("shot.png");
        let (tx, rx) = unbounded();
        assert!(ShotJob::prepare(target.clone(), tx).is_none());
        match rx.try_recv().expect("error must be signalled at once") {
            Err(ShotError::InvalidPath { path, reason }) => {
                assert_eq!(path, target);
                assert_eq!(reason, "parent directory does not exist");
            },
            other => panic!("unexpected signal {other:?}"),
        }
        assert!(rx.try_recv().is_err(), "exactly one signal");
        assert!(!dir.join("sub").exists(), "no directory may be created");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn complete_writes_png_and_signals_path() {
        let dir = scratch_dir("ok");
        let target = dir.join("citadel_noon.png");
        let (tx, rx) = unbounded();
        let job = ShotJob::prepare(target.clone(), tx).expect("valid path");
        assert!(rx.try_recv().is_err(), "nothing signalled before capture");
        job.complete(Ok(test_image()));
        assert_eq!(rx.try_recv().unwrap(), Ok(target.clone()));
        assert!(rx.try_recv().is_err(), "exactly one signal");

        let read = image::open(&target).expect("valid PNG").to_rgb8();
        assert_eq!(read, test_image());
        let names: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, vec![std::ffi::OsString::from("citadel_noon.png")]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn readback_error_is_signalled() {
        let dir = scratch_dir("readback");
        let (tx, rx) = unbounded();
        let job = ShotJob::prepare(dir.join("a.png"), tx).unwrap();
        job.complete(Err("device lost".into()));
        assert_eq!(
            rx.try_recv().unwrap(),
            Err(ShotError::Readback("device lost".into()))
        );
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn write_error_is_signalled_not_panicked() {
        let dir = scratch_dir("write");
        let target = dir.join("gone.png");
        let (tx, rx) = unbounded();
        let job = ShotJob::prepare(target.clone(), tx).unwrap();
        // The directory disappears between the request and the read-back.
        std::fs::remove_dir_all(&dir).unwrap();
        job.complete(Ok(test_image()));
        match rx.try_recv().unwrap() {
            Err(ShotError::Write { path, .. }) => assert_eq!(path, target),
            other => panic!("unexpected signal {other:?}"),
        }
        assert!(!dir.exists(), "the directory must not be recreated");
    }

    #[test]
    fn dropped_request_still_signals() {
        let dir = scratch_dir("dropped");
        let target = dir.join("b.png");
        let (tx, rx) = unbounded();
        drop(ShotJob::prepare(target.clone(), tx).unwrap());
        assert_eq!(
            rx.try_recv().unwrap(),
            Err(ShotError::Dropped { path: target })
        );
        assert!(rx.try_recv().is_err(), "exactly one signal");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// File names in `dir`, sorted.
    fn names(dir: &Path) -> Vec<String> {
        let mut v: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    }

    #[test]
    fn full_channel_does_not_block() {
        let dir = scratch_dir("full");
        let (tx, rx) = crossbeam_channel::bounded(1);
        tx.send(Ok(PathBuf::from("/occupied"))).unwrap();
        let job = ShotJob::prepare(dir.join("d.png"), tx.clone()).unwrap();
        // Would deadlock with a blocking send: nobody receives on this thread.
        job.complete(Ok(test_image()));
        drop(ShotJob::prepare(dir.join("e.png"), tx).unwrap());
        assert_eq!(rx.try_recv().unwrap(), Ok(PathBuf::from("/occupied")));
        assert!(rx.try_recv().is_err(), "results beyond capacity are lost");
        assert!(dir.join("d.png").is_file(), "the file is still written");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn overlapping_jobs_for_one_path_both_succeed() {
        let dir = scratch_dir("overlap");
        let target = dir.join("same.png");
        let (tx, rx) = unbounded();
        let a = ShotJob::prepare(target.clone(), tx.clone()).unwrap();
        let b = ShotJob::prepare(target.clone(), tx).unwrap();
        let ta = std::thread::spawn(move || a.complete(Ok(test_image())));
        let tb = std::thread::spawn(move || b.complete(Ok(test_image())));
        ta.join().unwrap();
        tb.join().unwrap();
        assert_eq!(rx.try_recv().unwrap(), Ok(target.clone()));
        assert_eq!(rx.try_recv().unwrap(), Ok(target.clone()));
        assert_eq!(image::open(&target).unwrap().to_rgb8(), test_image());
        assert_eq!(names(&dir), vec!["same.png"], "no temporary file left");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn existing_file_is_overwritten() {
        let dir = scratch_dir("overwrite");
        let target = dir.join("old.png");
        std::fs::write(&target, b"not a png").unwrap();
        let (tx, rx) = unbounded();
        ShotJob::prepare(target.clone(), tx)
            .unwrap()
            .complete(Ok(test_image()));
        assert_eq!(rx.try_recv().unwrap(), Ok(target.clone()));
        assert_eq!(image::open(&target).unwrap().to_rgb8(), test_image());
        assert_eq!(names(&dir), vec!["old.png"]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn stale_partial_files_are_left_alone() {
        let dir = scratch_dir("stale");
        let target = dir.join("s.png");
        // Leftovers from a crashed run, under the old and the current scheme.
        std::fs::write(dir.join("s.png.partial"), b"stale").unwrap();
        std::fs::write(dir.join("s.png.1.0.partial"), b"stale").unwrap();
        let (tx, rx) = unbounded();
        ShotJob::prepare(target.clone(), tx)
            .unwrap()
            .complete(Ok(test_image()));
        assert_eq!(rx.try_recv().unwrap(), Ok(target.clone()));
        assert_eq!(image::open(&target).unwrap().to_rgb8(), test_image());
        assert_eq!(names(&dir), vec![
            "s.png",
            "s.png.1.0.partial",
            "s.png.partial"
        ]);
        assert_eq!(std::fs::read(dir.join("s.png.partial")).unwrap(), b"stale");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn existing_temporary_name_fails_cleanly() {
        let dir = scratch_dir("collide");
        let target = dir.join("c.png");
        // Plant files at the next several temporary names, so the job hits
        // one even if other tests advance the counter meanwhile.
        let start = PARTIAL_COUNTER.load(Ordering::Relaxed);
        let planted: Vec<PathBuf> = (start..start + 64)
            .map(|n| {
                let p = format!("{}.{}.{n}.partial", target.display(), std::process::id());
                std::fs::write(&p, b"keep").unwrap();
                PathBuf::from(p)
            })
            .collect();
        let (tx, rx) = unbounded();
        ShotJob::prepare(target.clone(), tx)
            .unwrap()
            .complete(Ok(test_image()));
        match rx.try_recv().unwrap() {
            Err(ShotError::Write { path, error }) => {
                assert_eq!(path, target);
                assert!(error.contains("couldn't create"), "{error}");
            },
            other => panic!("unexpected signal {other:?}"),
        }
        assert!(!target.exists());
        for p in &planted {
            assert_eq!(std::fs::read(p).unwrap(), b"keep");
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn planted_symlink_is_not_followed() {
        let dir = scratch_dir("symlink");
        let victim = dir.join("victim.txt");
        std::fs::write(&victim, b"untouched").unwrap();
        let target = dir.join("l.png");
        // Plant symlinks at the next several temporary names, so the job hits
        // one even if other tests advance the counter meanwhile.
        let start = PARTIAL_COUNTER.load(Ordering::Relaxed);
        for n in start..start + 64 {
            let link = format!("{}.{}.{n}.partial", target.display(), std::process::id());
            std::os::unix::fs::symlink(&victim, link).unwrap();
        }
        let (tx, rx) = unbounded();
        ShotJob::prepare(target.clone(), tx)
            .unwrap()
            .complete(Ok(test_image()));
        assert!(matches!(
            rx.try_recv().unwrap(),
            Err(ShotError::Write { .. })
        ));
        assert_eq!(std::fs::read(&victim).unwrap(), b"untouched");
        assert!(!target.exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn gone_receiver_does_not_panic() {
        let dir = scratch_dir("norx");
        let (tx, rx) = unbounded();
        let job = ShotJob::prepare(dir.join("c.png"), tx).unwrap();
        drop(rx);
        job.complete(Ok(test_image()));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
