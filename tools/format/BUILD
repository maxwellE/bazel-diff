load("@aspect_rules_lint//format:defs.bzl", "format_multirun")

# buildifier-only formatter for Starlark (BUILD, BUILD.bazel, *.bzl, MODULE.bazel)
# files, driven by its own daily cron (.github/workflows/buildifier.yaml). Kept
# separate from the `rustfmt` target so the two never reformat each other's files.
format_multirun(
    name = "buildifier",
    starlark = "@buildifier_prebuilt//:buildifier",
    visibility = ["//visibility:public"],
)

# The fixer for the rustfmt gate. //:rust_format_check only reports (it fails
# the build, it does not rewrite files), so `bazel run //tools/format:rustfmt`
# (or `make format`) is how you satisfy it. One language per target, so a cron
# on one never rewrites another's files.
#
# `upstream_wrapper:rustfmt` is plain rustfmt from the registered toolchain --
# not //tools/rustfmt, which is a Bazel-target-driven wrapper that does not take
# the file list aspect_rules_lint passes it.
format_multirun(
    name = "rustfmt",
    rust = "@rules_rust//tools/upstream_wrapper:rustfmt",
    visibility = ["//visibility:public"],
)
