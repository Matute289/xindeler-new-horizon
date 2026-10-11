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

use crossbeam_channel::Sender;
use std::{
    fmt,
    path::{Component, Path, PathBuf},
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

/// Check a screenshot target. The script layer already confines paths to its
/// output directory; this is a second, independent guard: the path must be
/// absolute, contain no `..`, name a `.png` file, and its parent directory
/// must already exist (no directory is ever created here).
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

impl DoneSignal {
    fn send(mut self, result: ShotResult) {
        if let Some(tx) = self.tx.take() {
            // The receiver may be gone (driver shut down); nothing to do then.
            let _ = tx.send(result);
        }
    }
}

impl Drop for DoneSignal {
    fn drop(&mut self) {
        if let Some(tx) = self.tx.take() {
            let _ = tx.send(Err(ShotError::Dropped {
                path: std::mem::take(&mut self.path),
            }));
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

/// Encode `image` as PNG next to `path` and move it into place, so a reader
/// never sees a half-written file at `path`.
fn write_png(image: &image::RgbImage, path: &Path) -> ShotResult {
    let write_err = |error: String| ShotError::Write {
        path: path.to_path_buf(),
        error,
    };
    let mut partial = path.as_os_str().to_owned();
    partial.push(".partial");
    let partial = PathBuf::from(partial);

    if let Err(e) = image.save_with_format(&partial, image::ImageFormat::Png) {
        let _ = std::fs::remove_file(&partial);
        tracing::warn!(?e, ?path, "Couldn't save shot-script screenshot");
        return Err(write_err(e.to_string()));
    }
    if let Err(e) = std::fs::rename(&partial, path) {
        let _ = std::fs::remove_file(&partial);
        tracing::warn!(?e, ?path, "Couldn't move shot-script screenshot into place");
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
