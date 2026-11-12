#!/bin/bash
# cargo clippy is a superset of cargo check,
# so we don't check manually.

# Dev-only features must stay opt-in. `shot-script` (unattended screenshot
# driver) may only be enabled explicitly: never by another feature of
# xindeler-voxygen (in particular `default` / `default-publish`), and never by
# another workspace manifest depending on xindeler-voxygen with it. The Rust
# test `feature_guard_tests` checks the transitive case; this dependency-free
# gate makes the common mistakes fail before anything compiles.
check_explicit_only_features() {
    local manifest=voxygen/Cargo.toml status=0 offenders other
    # Any quoted "shot-script" (basic "..." or literal '...' TOML string, also
    # as "xindeler-voxygen/shot-script" or similar) inside [features] other
    # than the definition `shot-script = []` itself would make some other
    # feature enable it.
    offenders=$(awk '
        /^\[/ { in_features = ($0 ~ /^\[features\]/); next }
        in_features {
            line = $0; sub(/#.*/, "", line)
            if (line ~ /["\047]([^"\047]*\/)?shot-script["\047]/) print NR ": " $0
        }' "$manifest")
    if [ -n "$offenders" ]; then
        echo "error: a feature of $manifest enables \"shot-script\" (it must stay explicit-only):" >&2
        echo "$offenders" >&2
        status=1
    fi
    # Other manifests naming xindeler-voxygen together with the feature.
    other=$(grep -rl --include=Cargo.toml 'xindeler-voxygen' . 2>/dev/null \
        | grep -v -e '^\./voxygen/Cargo.toml$' -e '^\./target/' \
        | while IFS= read -r f; do grep -l 'shot-script' "$f" 2>/dev/null; done)
    if [ -n "$other" ]; then
        echo "error: these manifests depend on xindeler-voxygen with \"shot-script\":" >&2
        echo "$other" >&2
        status=1
    fi
    return $status
}

check_explicit_only_features &&

time cargo clippy \
    --all-targets \
    --locked \
    --features="bin_cmd_doc_gen,bin_compression,bin_csv,bin_graphviz,bin_bot,bin_asset_migrate,bin,stat,cli" \
    -- -D warnings &&

# Ensure that the xindeler-voxygen default-publish feature builds as it excludes some default features.
time cargo clippy -p \
    xindeler-voxygen --locked \
    --no-default-features \
    --features="default-publish" \
    -- -D warnings &&

# Ensure that the opt-in dev-only shot-script feature (never in a default set)
# still compiles, including its tests.
time cargo clippy -p \
    xindeler-voxygen --locked \
    --all-targets \
    --features="shot-script" \
    -- -D warnings &&

# Ensure that test-server compiles.
time cargo clippy --locked --bin xindeler-server-cli --no-default-features -F simd  -- -D warnings &&
# Ensure the opt-in airship route map renderer (`airship_maps`, off in every
# other set above) still compiles and is clippy-clean, including its tests.
time cargo clippy -p xindeler-world --locked \
    --all-targets \
    --features="airship_maps" \
    -- -D warnings &&

# Run the world and terrain-probe unit tests. This checkout has no LFS, so the
# asset-loader canary file is a pointer that would abort start-up: satisfy it
# first, but only when it really is a pointer (a local run with LFS must not
# have its canary rewritten). The tests that need the `.vox` binaries are
# `#[ignore]`d with that reason, and the chunk-generation golden test skips
# itself without them. `economy::context::tests::test_economy_moderate_standalone`
# is an upstream simulation that took ~4 minutes alone without the LFS assets
# (it guards the economy, not this code) and is skipped here, as is
# `sim::tests::cromatolis_world_generates_without_lfs_assets` (~5 minutes: it
# generates the whole authored world; run it locally with
# `cargo test -p xindeler-world --lib cromatolis_world_generates_without_lfs_assets`).
# XINDELER_SKIP_CI_TESTS=1 skips this test step (clippy and fmt still run);
# unset, the tests run.
if [ "${XINDELER_SKIP_CI_TESTS:-0}" = "1" ]; then
    echo "XINDELER_SKIP_CI_TESTS=1: skipping the world and terrain-probe unit tests"
else
    if head -c 7 assets/common/canary.canary | grep -q '^version'; then
        printf 'VELOREN_CANARY_MAGIC\n' > assets/common/canary.canary
    fi &&
    time cargo test --locked -p xindeler-world --lib \
        -- --skip test_economy_moderate_standalone \
        --skip cromatolis_world_generates_without_lfs_assets &&
    # terrain-probe is a binary crate: its tests are the bin's.
    time cargo test --locked -p xindeler-terrain-probe --bins
fi &&

time cargo fmt --all -- --check;
