"""Bazel labels for the third-party crates Cargo.toml declares.

Keep these lists in sync with the [dependencies], [build-dependencies] and
[dev-dependencies] tables of the root Cargo.toml: a crate missing here fails
the Bazel build with an unresolved-crate error rather than going unnoticed.

The labels are the unversioned aliases rules_rs's `crate.from_cargo` hub
(@bazel_diff_crates, see MODULE.bazel) generates for every direct dependency,
so a Cargo.lock bump needs no edit here. They are spelled out rather than
taken from the hub's `all_crate_deps()` because that helper looks dependencies
up by the calling BUILD file's package, and the Cargo package lives at the
repository root while its Bazel targets live under //src and //tests.
"""

CRATE_DEPS = [
    "@bazel_diff_crates//:anyhow",
    "@bazel_diff_crates//:buffa",
    "@bazel_diff_crates//:clap",
    "@bazel_diff_crates//:hex",
    "@bazel_diff_crates//:mimalloc",
    "@bazel_diff_crates//:rayon",
    "@bazel_diff_crates//:rust-s3",
    "@bazel_diff_crates//:serde",
    "@bazel_diff_crates//:serde_json",
    "@bazel_diff_crates//:sha2",
    "@bazel_diff_crates//:tempfile",
    "@bazel_diff_crates//:tiny_http",
    "@bazel_diff_crates//:url",
    "@bazel_diff_crates//:walkdir",
]

# protoc-bin-vendored is left out on purpose: it is behind the Cargo-only
# `vendored-protoc` feature (see Cargo.toml), and //src:build_script gets protoc
# from @protobuf instead.
BUILD_DEPS = [
    "@bazel_diff_crates//:buffa-build",
]

DEV_DEPS = [
    "@bazel_diff_crates//:zip",
]
