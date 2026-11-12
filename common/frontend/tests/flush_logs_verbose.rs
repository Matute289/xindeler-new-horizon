//! With `logging-verbose`, `flush_logs` must also put the tail of the info log
//! and the telemetry sink on disk. Separate file: each integration test file
//! runs in its own process, so the global subscriber is initialised once.
#![cfg(feature = "logging-verbose")]

fn read_matching(dir: &std::path::Path, needle: &str) -> String {
    std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().contains(needle))
        .map(|e| std::fs::read_to_string(e.path()).unwrap_or_default())
        .collect::<String>()
}

#[test]
fn flush_logs_writes_info_and_telemetry_tails() {
    let dir = tempfile::tempdir().expect("temp dir");
    let _guards = xindeler_common_frontend::init_split_logs("server", dir.path());

    tracing::info!("info-flush-marker");
    tracing::info!(target: "telemetry", event = "flush_marker", seq = 1u64);

    xindeler_common_frontend::flush_logs();

    assert!(read_matching(dir.path(), "_info").contains("info-flush-marker"));
    assert!(read_matching(dir.path(), "_telemetry").contains("flush_marker"));
}
