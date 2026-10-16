//! `flush_logs` must put the tail of the file logs on disk: the file writers
//! are buffered inside the global subscriber, which is never dropped, so
//! without an explicit flush the last lines are lost when the process exits.

#[test]
fn flush_logs_writes_buffered_error_line_to_err_log() {
    let dir = tempfile::tempdir().expect("temp dir");
    let _guards = xindeler_common_frontend::init_split_logs("server", dir.path());

    tracing::error!("flush-regression-marker");

    let read_err_logs = || {
        std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().contains("_err"))
            .map(|e| std::fs::read_to_string(e.path()).unwrap_or_default())
            .collect::<String>()
    };
    assert!(
        !read_err_logs().contains("flush-regression-marker"),
        "precondition: the line is still buffered before flushing"
    );
    xindeler_common_frontend::flush_logs();
    assert!(read_err_logs().contains("flush-regression-marker"));
}
