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
time cargo fmt --all -- --check;
