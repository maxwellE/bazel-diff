# Platforms bazel-diff publishes release binaries for.
#
# Bazel has no built-in notion of which libc a Linux platform uses. rules_rs
# supplies one (@rules_rs//rs/platforms/constraints:linux_libc, glibc by
# default), and its Rust toolchains key the x86_64/aarch64 musl target triples
# off it. The musl C toolchain in MODULE.bazel is gated on the same constraint,
# so neither the musl Rust std nor the musl cross-compiler can leak into an
# ordinary (glibc) build.

package(default_visibility = ["//visibility:public"])

# Platforms the published Linux binaries are built for:
# `bazel build //release:bazel-diff-rust --config=release-musl` (amd64) and
# `--config=release-musl-arm64` (arm64). Statically linked against musl, so the
# binary does not depend on the glibc version of the machine that runs it.
# Reachable from a glibc Linux host and from an Apple Silicon Mac -- both the
# Rust std and the C toolchain cross-compile.
platform(
    name = "linux_x86_64_musl",
    constraint_values = [
        "@platforms//cpu:x86_64",
        "@platforms//os:linux",
        "@rules_rs//rs/platforms/constraints:musl",
    ],
)

platform(
    name = "linux_aarch64_musl",
    constraint_values = [
        "@platforms//cpu:aarch64",
        "@platforms//os:linux",
        "@rules_rs//rs/platforms/constraints:musl",
    ],
)

# Host platform for Windows builds (set in .bazelrc; BCR's Windows presubmit
# passes it too). rules_rs selects Windows Rust toolchains by ABI, and the
# autodetected host platform leaves the ABI unconstrained, so no
# x86_64-pc-windows-msvc toolchain would match.
platform(
    name = "local_windows_msvc",
    constraint_values = ["@llvm//constraints/windows/abi:msvc"],
    parents = ["@platforms//host"],
)
