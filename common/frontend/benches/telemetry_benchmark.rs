use criterion::{Criterion, criterion_group, criterion_main};
use tracing_subscriber::prelude::*;
use xindeler_common_frontend::TelemetryLayer;

pub fn criterion_benchmark(c: &mut Criterion) {
    let dir = std::env::temp_dir().join(format!("veloren-telemetry-bench-{}", std::process::id()));
    let layer = TelemetryLayer::new(&dir, "bench").expect("create telemetry file");
    let _guard = tracing::subscriber::set_default(tracing_subscriber::registry().with(layer));

    // Mirrors a combat telemetry!() call site (cf. common/systems/src/melee.rs)
    c.bench_function("telemetry_on_event", |b| {
        b.iter(|| {
            tracing::info!(
                target: "telemetry",
                event = "melee_hit",
                attacker = 42_u64,
                damage = 23.456_f64,
                ability = "sword_basic \"combo\"",
            );
        })
    });
    drop(_guard);
    let _ = std::fs::remove_dir_all(&dir);
}

criterion_group!(benches, criterion_benchmark);
criterion_main!(benches);
