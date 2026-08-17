//! Unit tests for the packaging authoring that no other test can reach.
//!
//! `packaging/windows/Package.wxs` is read by exactly one thing — WiX, on a
//! Windows runner, during a release build. A malformed character in it is
//! therefore found after a tag is pushed, when the fix costs a new version
//! rather than a new commit (features/packaging-and-release.md phase 4). These
//! tests read the file as text on any platform, so the cheap classes of mistake
//! fail in `cargo test` instead.

use std::path::PathBuf;

fn packaging_file(relative: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(relative);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// XML forbids `--` inside a comment, and a comment may not end on `-`. WiX
/// rejects the file wholesale for it (`error WIX0104`), so a command-line flag
/// written into a comment the obvious way — with its two leading hyphens —
/// breaks the installer build and nothing else.
#[test]
fn the_wix_authoring_has_no_illegal_comment() {
    let source = packaging_file("packaging/windows/Package.wxs");

    let mut rest = source.as_str();
    let mut offset = 0usize;
    while let Some(start) = rest.find("<!--") {
        let after_open = start + "<!--".len();
        let body_start = offset + after_open;
        let tail = &rest[after_open..];
        let end = tail
            .find("-->")
            .unwrap_or_else(|| panic!("unterminated XML comment at byte {body_start}"));
        let body = &tail[..end];

        let line = source[..body_start].lines().count();
        assert!(
            !body.contains("--"),
            "packaging/windows/Package.wxs:{line}: an XML comment cannot contain `--`, \
             and WiX refuses the whole file for it: {}",
            body.trim()
        );
        assert!(
            !body.ends_with('-'),
            "packaging/windows/Package.wxs:{line}: an XML comment cannot end on `-`: {}",
            body.trim()
        );

        offset = body_start + end + "-->".len();
        rest = &source[offset..];
    }
}
