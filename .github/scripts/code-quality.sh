#!/bin/bash
# cargo clippy is a superset of cargo check,
# so we don't check manually.

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
# (it guards the economy, not this code) and is skipped here.
if head -c 7 assets/common/canary.canary | grep -q '^version'; then
    printf 'VELOREN_CANARY_MAGIC\n' > assets/common/canary.canary
fi &&
time cargo test --locked -p xindeler-world --lib \
    -- --skip test_economy_moderate_standalone &&
# terrain-probe is a binary crate: its tests are the bin's.
time cargo test --locked -p xindeler-terrain-probe --bins &&

time cargo fmt --all -- --check;
