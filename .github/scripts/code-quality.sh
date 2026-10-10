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
time cargo fmt --all -- --check;
