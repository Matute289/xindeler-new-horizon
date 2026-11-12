use chrono::Utc;
use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, bounded};
use std::{
    fmt::Write as _,
    fs::{self, File},
    io::{BufWriter, Write},
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    thread,
    time::Duration,
};
use tracing::{
    Event, Subscriber,
    field::{Field, Visit},
};
use tracing_subscriber::{Layer, layer::Context};

/// Max queued lines; beyond this events are dropped (and counted) rather
/// than blocking the emitting thread. 1024 × ~100B lines ≈ 100KB worst case.
const QUEUE_CAPACITY: usize = 1024;
const FLUSH_INTERVAL: Duration = Duration::from_millis(250);

enum Msg {
    Line(String),
    /// Flush the writer and ack — used by tests and the shutdown path.
    Flush(Sender<()>),
}

pub struct TelemetryLayer {
    tx: Sender<Msg>,
    recycle_rx: Receiver<String>,
    dropped: Arc<AtomicU64>,
}

/// Cloneable handle to flush the drain thread (held by `LogGuards` so the
/// last batch is written on shutdown).
#[derive(Clone)]
pub struct TelemetryFlushHandle {
    tx: Sender<Msg>,
}

impl TelemetryFlushHandle {
    /// Blocks until everything queued before this call is on disk (5s
    /// timeout so a dead drain thread cannot hang process exit).
    pub fn flush(&self) {
        let (ack_tx, ack_rx) = bounded(1);
        if self.tx.send(Msg::Flush(ack_tx)).is_ok() {
            let _ = ack_rx.recv_timeout(Duration::from_secs(5));
        }
    }
}

impl TelemetryLayer {
    /// Returns `None` if the file cannot be created (logs a warning, doesn't
    /// panic).
    pub fn new(logs_dir: &Path, prefix: &str) -> Option<Self> {
        let _ = fs::create_dir_all(logs_dir);
        let bucket = Utc::now().format("%Y-%m-%d_%Hh").to_string();
        let name = format!("{bucket}_{prefix}_telemetry.jsonl");
        let file = match File::create(logs_dir.join(&name)) {
            Ok(f) => f,
            Err(e) => {
                eprintln!("[log] Failed to create telemetry file {name}: {e}");
                return None;
            },
        };

        let (tx, rx) = bounded::<Msg>(QUEUE_CAPACITY);
        let (recycle_tx, recycle_rx) = bounded::<String>(QUEUE_CAPACITY);
        let dropped = Arc::new(AtomicU64::new(0));
        let dropped_for_drain = Arc::clone(&dropped);
        if let Err(e) = thread::Builder::new()
            .name("telemetry-drain".into())
            .spawn(move || drain(rx, recycle_tx, BufWriter::new(file), dropped_for_drain))
        {
            eprintln!("[log] Failed to spawn telemetry drain thread: {e}");
            return None;
        }

        Some(Self {
            tx,
            recycle_rx,
            dropped,
        })
    }

    pub fn flush_handle(&self) -> TelemetryFlushHandle {
        TelemetryFlushHandle {
            tx: self.tx.clone(),
        }
    }

    /// Events dropped because the queue was full (visible failure mode).
    pub fn dropped_events(&self) -> u64 { self.dropped.load(Ordering::Relaxed) }
}

fn drain(
    rx: Receiver<Msg>,
    recycle_tx: Sender<String>,
    mut writer: BufWriter<File>,
    dropped: Arc<AtomicU64>,
) {
    let mut reported_dropped = 0u64;
    loop {
        match rx.recv_timeout(FLUSH_INTERVAL) {
            Ok(Msg::Line(line)) => {
                let _ = writer.write_all(line.as_bytes());
                // Return the buffer for reuse; drop it if the pool is full.
                let _ = recycle_tx.try_send(line);
            },
            Ok(Msg::Flush(ack)) => {
                let _ = writer.flush();
                let _ = ack.send(());
            },
            Err(RecvTimeoutError::Timeout) => {
                let total = dropped.load(Ordering::Relaxed);
                if total > reported_dropped {
                    let ts = Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ");
                    let _ = writeln!(
                        writer,
                        "{{\"ts\":\"{ts}\",\"event\":\"telemetry_dropped\",\"total_dropped\":\
                         {total}}}"
                    );
                    reported_dropped = total;
                }
                let _ = writer.flush();
            },
            Err(RecvTimeoutError::Disconnected) => {
                let _ = writer.flush();
                return;
            },
        }
    }
}

impl<S: Subscriber> Layer<S> for TelemetryLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        if event.metadata().target() != "telemetry" {
            return;
        }

        // Reuse a drained buffer when available (zero allocation steady
        // state); only the first QUEUE_CAPACITY events allocate.
        let mut line = self.recycle_rx.try_recv().unwrap_or_default();
        line.clear();

        let ts = Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ");
        let _ = write!(line, "{{\"ts\":\"{ts}\"");
        event.record(&mut JsonVisitor { line: &mut line });
        line.push_str("}\n");

        // Never block the emitting (tick) thread: drop on full and count.
        if self.tx.try_send(Msg::Line(line)).is_err() {
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }
}

struct JsonVisitor<'a> {
    line: &'a mut String,
}

/// `fmt::Write` adapter that JSON-escapes as it writes — lets `record_debug`
/// format directly into the line buffer with no intermediate `String`.
struct EscapingWriter<'a>(&'a mut String);

impl std::fmt::Write for EscapingWriter<'_> {
    fn write_str(&mut self, s: &str) -> std::fmt::Result {
        for c in s.chars() {
            match c {
                '"' => self.0.push_str("\\\""),
                '\\' => self.0.push_str("\\\\"),
                '\n' => self.0.push_str("\\n"),
                c if c.is_control() => write!(self.0, "\\u{:04x}", c as u32)?,
                c => self.0.push(c),
            }
        }
        Ok(())
    }
}

impl Visit for JsonVisitor<'_> {
    fn record_str(&mut self, field: &Field, value: &str) {
        let _ = write!(self.line, ",\"{}\":\"", field.name());
        let _ = EscapingWriter(self.line).write_str(value);
        self.line.push('"');
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        let _ = write!(self.line, ",\"{}\":\"", field.name());
        let _ = write!(EscapingWriter(self.line), "{value:?}");
        self.line.push('"');
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        let _ = write!(self.line, ",\"{}\":{}", field.name(), value);
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        let _ = write!(self.line, ",\"{}\":{}", field.name(), value);
    }

    fn record_f64(&mut self, field: &Field, value: f64) {
        let _ = write!(self.line, ",\"{}\":{:.3}", field.name(), value);
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        let _ = write!(self.line, ",\"{}\":{}", field.name(), value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracing_subscriber::prelude::*;

    #[test]
    fn events_reach_disk_in_order_with_escaping() {
        let dir =
            std::env::temp_dir().join(format!("veloren-telemetry-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let layer = TelemetryLayer::new(&dir, "test").expect("layer");
        let flush = layer.flush_handle();
        tracing::subscriber::with_default(tracing_subscriber::registry().with(layer), || {
            for i in 0..100u64 {
                tracing::info!(
                    target: "telemetry",
                    event = "test_event",
                    seq = i,
                    label = "with \"quotes\" and\nnewline",
                );
            }
            // Different target: must NOT be written
            tracing::info!(other = true, "ordinary log line");
        });
        flush.flush();

        let path = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .find(|p| p.extension().is_some_and(|e| e == "jsonl"))
            .expect("telemetry .jsonl exists");
        let content = std::fs::read_to_string(path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(
            lines.len(),
            100,
            "all telemetry events written, nothing else"
        );
        for (i, line) in lines.iter().enumerate() {
            assert!(line.starts_with("{\"ts\":\""), "bad start: {line}");
            assert!(line.ends_with('}'), "bad end: {line}");
            assert!(line.contains(&format!("\"seq\":{i}")), "order: {line}");
            assert!(
                line.contains(r#"with \"quotes\" and\nnewline"#),
                "escaping: {line}"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
