---
name: xindeler-review
description: Use before handing a branch off for review or opening a PR — runs CI lint checks, verifies ECS patterns, then invokes Superpowers code review. AI agents report findings and never merge.
---

# xindeler-review

**Run this skill before handing a branch to Matias or opening its PR.** It does not
merge or approve anything—it verifies and reports.

## Step 1: Check Formatting

```bash
source "$HOME/.cargo/env"
cargo fmt --all -- --check
```

- **Pass (no output, exit 0):** Formatting is clean. Continue.
- **Fail (lists files):** Run `cargo fmt --all` to fix, then review the diff before continuing.

## Step 2: Lint — All Targets (CI exact command)

```bash
cargo ci-clippy
# expands to:
# cargo clippy --all-targets --locked \
#   --features="bin_cmd_doc_gen,bin_compression,bin_csv,bin_graphviz,bin_bot,bin_asset_migrate,asset_tweak,bin,stat"
```

Fix every warning before proceeding. To treat warnings as errors (strict mode, matching CI):
```bash
cargo ci-clippy -- -D warnings
```

## Step 3: Lint — Voxygen Publish Profile (CI exact command)

```bash
cargo ci-clippy2
# expands to:
# cargo clippy -p xindeler-voxygen --locked \
#   --no-default-features --features="default-publish"
```

This checks the client in release mode (no hot-reloading). Catches feature-gated issues that only surface in publish builds.

## Step 4: ECS Pattern Checklist

For each new component, system, or resource added in this diff, verify:

**New ECS components:**
- [ ] Struct derives `specs::Component` (usually via `#[derive(Component)]`)
- [ ] Registered in `common/state/src/state.rs` via `ecs.register::<comp::MyComponent>()`
- [ ] Exported from `common/src/comp/mod.rs`

**New shared systems (client+server):**
- [ ] Implement `specs::System`
- [ ] Registered in `common/systems/src/lib.rs` via `dispatch::<Sys>(dispatch_builder, &[deps])`
- [ ] Dependencies declared correctly (systems that write to components this system reads are listed as deps)

**New server-only systems:**
- [ ] Implement `specs::System`
- [ ] Added to the server dispatcher in `server/src/sys/mod.rs`

**New resources:**
- [ ] Defined in `common/src/resources.rs`
- [ ] Inserted into the world in `common/state/src/state.rs` via `ecs.insert(...)`

**New admin commands:**
- [ ] Variant added to `ServerChatCommand` enum in `common/src/cmd.rs`
- [ ] Handler implemented in `server/src/cmd.rs`
- [ ] Help text added to the command definition

## Step 5: Invoke Code Review

```
superpowers:requesting-code-review
```

This does the deep analysis of the diff — logic errors, edge cases, performance, security. The steps above are pre-checks so the code review focuses on logic, not style.

## Step 6: After Review Feedback

If the code review or CI finds issues:
1. Fix the issues.
2. Repeat Steps 1–3 (fmt + clippy) after changes.
3. Do not merge until all CI checks pass and review is approved.
