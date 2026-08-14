# `block` 0.1.6, with one lint fixed

A verbatim copy of [`block` 0.1.6](https://crates.io/crates/block/0.1.6) by
Steven Sheldon, MIT licensed (see `LICENSE`), with the smallest change that
removes a future-incompatibility warning. It is applied through
`[patch.crates-io]` in the workspace manifest.

## Why it is here

`block` reaches this project through `nokhwa` → `nokhwa-bindings-macos`, which
uses Objective-C blocks for AVFoundation's capture callbacks. `camera` is a
default feature, so **every artefact this project ships** carries `block` — and
cargo reports it as containing code that a future rustc will reject:

    warning: static of uninhabited type
      --> block-0.1.6/src/lib.rs:64:5
       |
    64 |     static _NSConcreteStackBlock: Class;

`features/packaging-and-release.md` phase 0b records this as a release blocker
under NRM §5.4.3, and lists four ways out. Three of them were unavailable or
disproportionate:

* **an upstream fix** — `block` has had no release since 2020, and the ecosystem
  moved to `block2`, which is not API-compatible with what `nokhwa` calls;
* **a native AVFoundation capture path** in this repository — weeks of platform
  code to replace a dependency that works;
* **making `camera` opt-in again** — reverses the decision of 2026-08-10, which
  was made so that an operator does not need a special build to point a webcam
  at a box label. That reversal is the owner's to make, not an implementer's.

The fourth is the one cargo itself suggests for a dependency whose maintainer
cannot be waited on, and is what this directory is.

## The change that matters, in full

`Class` is a private, opaque type inside `block`. It exists only so a pointer to
the runtime's `_NSConcreteStackBlock` can be stored in a block's `isa` field, and
no code in the crate ever reads through it. Upstream declares it uninhabited:

```rust
enum Class { }
```

Here it is a zero-sized `#[repr(C)]` struct, which is inhabited and therefore
legal as the type of an `extern` static:

```rust
#[repr(C)]
struct Class {
    _opaque: [u8; 0],
}
```

Two other differences, and neither changes what the crate does:

* **`extern` became `extern "C"`** in ten places, applied by `cargo fix`. `"C"`
  is the ABI those declarations already had — it is the default — and writing it
  out silences the `missing_abi` deprecation. That matters here and not in a
  registry dependency: cargo caps lints for crates it downloads, and does not for
  a patched path crate, so without this every build of this project would carry 46
  warnings and CI (`RUSTFLAGS: -D warnings`) would fail on them.
* **The test module is not carried over**, along with its `mod test_utils;`
  declaration: they call into an `objc_test_utils` helper crate that the
  *published* package excludes, so they cannot be built from a copy of the crate
  at all.

`README.upstream.md` is the crate's own README, kept for provenance.

## How to check this claim, and how to undo it

```bash
# what changed against the published crate
diff -u ~/.cargo/registry/src/*/block-0.1.6/src/lib.rs vendor/block/src/lib.rs

# the warning is gone
cargo build 2>&1 | grep -c 'future version of Rust'   # 0
```

To revert: delete this directory and the `[patch.crates-io]` section from
`Cargo.toml`. Nothing in this project's own code refers to `block`.
