load("//release:defs.bzl", "RELEASE_ASSET_NAME", "release_binary")

package(default_visibility = ["//visibility:public"])

# Built by the `release-artifacts` job in ci.yaml and the `binaries` job in
# release.yaml, which upload the file it writes to bazel-bin/release/ as-is.
release_binary(
    name = "bazel-diff",
    asset_name = RELEASE_ASSET_NAME,
    binary = "//src:bazel-diff",
)
