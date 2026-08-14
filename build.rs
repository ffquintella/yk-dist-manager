//! Build script: record **which commit this binary was built from**
//! (`features/packaging-and-release.md` phase 2).
//!
//! The norm's requirement is that every version installed anywhere is generated
//! from version control and carries a tag. A version number alone cannot show
//! that: `0.13.0` is what the working tree says, and it says the same thing on a
//! tagged build, on a build from an unrelated branch, and on a build with
//! uncommitted changes in it. So the binary carries the commit, and says when the
//! tree it came from was dirty — which is exactly the build that must never reach
//! an operator's workstation.
//!
//! Three rules this follows, and each one is about not breaking a build:
//!
//! 1. **A missing `git` is not an error.** The crate has to build from a source
//!    tarball, in a container without `git`, and inside a vendored dependency
//!    tree. Any of those produces `unknown`, which is honest and unambiguous.
//! 2. **Nothing is executed through a shell.** `Command` takes an argv vector,
//!    the way every subprocess in this codebase does (AGENTS.md §2) — there is no
//!    string interpolation here for anything to be injected into.
//! 3. **The output is bounded and sanitised.** Whatever `git` said is cut to the
//!    characters a commit id and a `-dirty` suffix are made of, so a hostile or
//!    broken `git` on `PATH` cannot inject `cargo:` directives into the build.
//!
//! One limitation, stated rather than discovered: the `-dirty` marker is as of the
//! last time this script ran, and it re-runs when the commit moves rather than
//! when a source file changes — so an incremental developer build can carry a
//! clean marker with edited sources. That is the case where it does not matter.
//! The case where it does is a release, and a release is built from a fresh
//! checkout in CI (`.github/workflows/release.yml`), where this runs once against
//! exactly the tree being shipped.

use std::process::Command;

/// The value used when nothing can be determined. Not an empty string: an
/// operator reading a support report needs to see that the field was answered.
const UNKNOWN: &str = "unknown";

fn main() {
    // Rebuild when the checked-out commit moves. `.git/HEAD` covers a checkout or
    // a commit on a detached head; the ref file covers a commit on a branch.
    if let Some(git_dir) = capture(&["rev-parse", "--absolute-git-dir"]) {
        println!("cargo:rerun-if-changed={git_dir}/HEAD");
        if let Some(head_ref) = capture(&["symbolic-ref", "-q", "HEAD"]) {
            println!("cargo:rerun-if-changed={git_dir}/{head_ref}");
        }
    }

    println!("cargo:rustc-env=YKDM_COMMIT={}", commit());
}

/// `<short commit>`, `<short commit>-dirty`, or `unknown`.
fn commit() -> String {
    let Some(commit) = capture(&["rev-parse", "--short=12", "HEAD"]) else {
        return UNKNOWN.to_owned();
    };
    // An empty `status --porcelain` is a clean tree. A `git` that failed here is
    // *not* reported as clean: not knowing is closer to dirty than to clean, and
    // the release check would rather look at a suspect build than miss one.
    match capture_allowing_empty(&["status", "--porcelain", "--untracked-files=no"]) {
        Some(changes) if changes.is_empty() => commit,
        _ => format!("{commit}-dirty"),
    }
}

fn capture(args: &[&str]) -> Option<String> {
    capture_allowing_empty(args).filter(|value| !value.is_empty())
}

/// Run `git` with an argv vector and return its sanitised stdout.
fn capture_allowing_empty(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let raw = String::from_utf8_lossy(&output.stdout);
    // Only what a commit id, a ref path and a porcelain status line are made of.
    // Everything else is dropped rather than escaped, because the result is going
    // into a `cargo:` directive and a value that can contain a newline can write
    // one of those.
    let cleaned: String = raw
        .trim()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || "/._-".contains(*c))
        .take(256)
        .collect();
    Some(cleaned)
}
