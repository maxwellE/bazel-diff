load("@rules_license//rules:license.bzl", "license")
load("@rules_rust//rust:defs.bzl", "rust_clippy_test", "rustfmt_test")

exports_files(
    [
        "build.rs",
        "rust/proto/analysis_v2.proto",
        "rust/proto/build.proto",
    ],
    visibility = ["//src:__pkg__"],
)

# The bazel-diff CLI. `bazel run //:bazel-diff -- --help`.
alias(
    name = "bazel-diff",
    actual = "//src:bazel-diff",
)

# Kept as a second name for the same binary: it is what the BCR presubmit's
# verify_targets, the release rule under //release and existing consumers
# build, from when the JVM CLI owned the `bazel-diff` name and this was the
# Rust candidate. Both aliases resolve to the one implementation.
alias(
    name = "bazel-diff-rust",
    actual = "//src:bazel-diff",
)

test_suite(
    name = "rust_tests",
    tests = [
        "//src:cli_tests",
        "//src:rust_tests",
    ],
)

# What CI runs in place of `cargo clippy --all-targets -- -D warnings` and
# `cargo fmt --all -- --check`.
#
# The .bazelrc aspects lint whatever a build names, which is the right tradeoff
# for local feedback but makes coverage a property of the command line: nothing
# in a build of //src:bazel-diff checks //tools/coverage. These two targets pin
# the roots instead, so the gate cannot quietly shrink when a CI command changes.
# `transitive` walks deps/crate from each root; external crates are skipped by
# the aspects themselves.
#
# Adding a first-party Rust crate? Add its root here.
_RUST_LINT_ROOTS = [
    "//src:bazel-diff",
    "//src:bazel_diff_lib",
    # The un-split, whole-crate e2e target (//tests:e2e_test is now a
    # test_suite over one target per case, and the per-case targets carry
    # no-clippy/no-rustfmt so the crate is linted once rather than 38 times).
    "//tests:e2e_test_all",
    "//tools/coverage:lcov_merger",
    "//tools/coverage:lcov_merger_test",
]

rust_clippy_test(
    name = "rust_clippy_check",
    targets = _RUST_LINT_ROOTS,
    transitive = True,
)

rustfmt_test(
    name = "rust_format_check",
    targets = _RUST_LINT_ROOTS,
    transitive = True,
)

# `bazel run //:format` rewrites the Rust sources with the pinned rustfmt; see
# //tools/format for the Starlark formatter.
alias(
    name = "format",
    actual = "//tools/format:rustfmt",
)

package(
    default_applicable_licenses = [":license"],
    default_visibility = ["//visibility:public"],
)

license(
    name = "license",
    package_name = "bazel-diff",
    copyright_notice = "Copyright (c) 2020, Match Group, LLC",
    license_kind = "@rules_license//licenses/spdx:BSD-3-Clause",
    license_text = "LICENSE",
    package_url = "https://github.com/Tinder/bazel-diff",
    package_version = "46.0.0",
)
