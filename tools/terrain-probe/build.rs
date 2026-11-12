//! Embeds the engine git commit into the binary (recorded in dump headers):
//! `<40 hex>` for a clean tree, `<40 hex>+dirty` when tracked files differ from
//! `HEAD`, `unknown` outside a git checkout.
//!
//! Everything is asked of git itself so it works in linked worktrees (where
//! `.git` is a file and the refs live in the common dir): `rev-parse HEAD` for
//! the id and `rev-parse --git-path <x>` for the files whose change must
//! re-run this script (`HEAD`, the branch ref it points at, `packed-refs`,
//! the index). The `+dirty` marker reflects the tree when the script last ran;
//! the crates the probe samples (`common`, `world`, `client`) and the probe's
//! own sources are watched too, so editing them re-runs it.
use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8(out.stdout).ok()?.trim().to_string())
}

fn watch_git_path(name: &str) {
    if let Some(p) = git(&["rev-parse", "--git-path", name]) {
        println!("cargo:rerun-if-changed={p}");
    }
}

fn main() {
    let commit = match git(&["rev-parse", "HEAD"]) {
        Some(sha) if !sha.is_empty() => {
            // `--no-optional-locks`: never write to the index from a build.
            let dirty = git(&[
                "--no-optional-locks",
                "status",
                "--porcelain",
                "--untracked-files=no",
            ])
            .is_some_and(|s| !s.is_empty());
            if dirty { format!("{sha}+dirty") } else { sha }
        },
        _ => "unknown".to_string(),
    };
    println!("cargo:rustc-env=TPROBE_ENGINE_COMMIT={commit}");

    watch_git_path("HEAD");
    if let Some(head_ref) = git(&["symbolic-ref", "-q", "HEAD"]) {
        watch_git_path(&head_ref);
    }
    watch_git_path("packed-refs");
    watch_git_path("index");
    for p in [
        "build.rs",
        "Cargo.toml",
        "src",
        "../../common",
        "../../world",
        "../../client",
    ] {
        println!("cargo:rerun-if-changed={p}");
    }
}
