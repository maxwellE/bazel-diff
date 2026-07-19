# Hermetic git for the serve capability.
#
# bazel-diff's `serve` mode shells out to a `git` binary (fetch, rev-parse,
# checkout; see cli/src/main/kotlin/com/bazel_diff/server/GitClient.kt), which
# by default must be preinstalled on the host. The targets here remove that host
# dependency: git is built from a pinned source release via bzlmod
# (//:extensions.bzl%hermetic_git) and rules_foreign_cc, and
# `bazel run //tools/git:serve` launches the query service with `--gitPath`
# already pointing at that binary.
#
# Scope: the hermetic git supports everything serve needs against local, ssh://
# and git:// remotes. It is built without libcurl (a transitive TLS stack is out
# of scope here), so it cannot talk to http(s):// remotes; point serve at a
# clone whose origin uses ssh, or keep using a system git via --gitPath for
# https remotes. Linux and macOS only, and bzlmod only: the @git_source /
# @zlib_source repos and the rules_foreign_cc / rules_shell dev deps are wired
# up in MODULE.bazel, not in the legacy WORKSPACE setup.

load("@rules_foreign_cc//foreign_cc:defs.bzl", "configure_make", "make")
load("@rules_shell//shell:sh_binary.bzl", "sh_binary")
load("@rules_shell//shell:sh_test.bzl", "sh_test")

# git's only mandatory library dependency. Built from source instead of using
# the bzlmod `zlib` module: git links with `-lz`, which needs a library named
# libz.a, while the module's cc_library produces libzlib.a. `--static` keeps a
# shared libz out of the install tree so git cannot pick up a runtime .so
# dependency on a path that only exists in the build sandbox.
configure_make(
    name = "zlib",
    args = ["-j4"],
    configure_in_place = True,
    configure_options = ["--static"],
    lib_source = "@zlib_source//:all_srcs",
    out_static_libs = ["libz.a"],
    visibility = ["//visibility:private"],
)

# Make variables for git's Makefile, passed on the make command line so they
# override any Makefile-internal assignment. The release tarball needs no
# autoconf: git's Makefile is self-sufficient, which is why this is a plain
# `make` build rather than configure_make.
_GIT_MAKE_SETTINGS = [
    # Install into the rules_foreign_cc install directory. RUNTIME_PREFIX makes
    # the binary relocatable: exec path, templates etc. are resolved relative to
    # the running executable instead of the compiled-in prefix, which is a
    # sandbox path that no longer exists at runtime.
    "prefix=$$INSTALLDIR$$",
    "RUNTIME_PREFIX=YesPlease",
    # The hermetic zlib above; git's Makefile appends $(ZLIB_PATH)/include and
    # $(ZLIB_PATH)/lib, matching the EXT_BUILD_DEPS layout.
    "ZLIB_PATH=$$EXT_BUILD_DEPS$$",
    # git's Makefile hardcodes `CC = cc`; respect the compiler rules_foreign_cc
    # exports from the configured Bazel C++ toolchain instead.
    "CC=${CC:-cc}",
    # git's Makefile also assigns CFLAGS itself (`-g -O2 -Wall`); override on
    # the command line to drop the debug info, which is ~5x of the install tree.
    "CFLAGS=\"-O2 -Wall\"",
    # Trim optional features and their library/tool dependencies to what serve
    # needs. NO_CURL drops http(s) remote support (see the header note);
    # NO_OPENSSL falls back to git's bundled collision-detecting SHA-1.
    "NO_CURL=YesPlease",
    "NO_EXPAT=YesPlease",
    "NO_OPENSSL=YesPlease",
    "NO_GETTEXT=YesPlease",
    "NO_ICONV=YesPlease",
    "NO_PERL=YesPlease",
    "NO_PYTHON=YesPlease",
    "NO_TCLTK=YesPlease",
    # Skip the ~150 deprecated dashed-builtin hardlinks in libexec/git-core;
    # everything serve invokes goes through the main `git` binary. Symlinks
    # instead of hardlinks for what remains, since hardlinks may not survive
    # copying out of the build sandbox.
    "SKIP_DASHED_BUILT_INS=YesPlease",
    "NO_INSTALL_HARDLINKS=YesPlease",
]

make(
    name = "git",
    args = ["-j4"] + _GIT_MAKE_SETTINGS,
    lib_source = "@git_source//:all_srcs",
    out_binaries = ["git"],
    visibility = ["//visibility:public"],
    deps = [":zlib"],
)

# The full install tree (bin/, libexec/git-core/, share/git-core/templates/).
# Always run git as <this directory>/bin/git: RUNTIME_PREFIX resolves the exec
# path relative to the executable, so the binary must sit inside its install
# tree, next to libexec/git-core and the helpers (git-upload-pack, ...) that
# fetch and clone spawn.
filegroup(
    name = "git_install_dir",
    srcs = [":git"],
    output_group = "gen_dir",
    visibility = ["//visibility:public"],
)

# Runs `bazel-diff serve` with --gitPath pointing at the hermetic git, e.g.:
#   bazel run //tools/git:serve -- -w /path/to/clone --cacheDir /path/to/cache
# An explicit --gitPath in the caller's arguments wins over the hermetic one.
sh_binary(
    name = "serve",
    srcs = ["serve_hermetic.sh"],
    args = [
        "$(rlocationpath :git_install_dir)",
        "$(rlocationpath //cli:bazel-diff)",
    ],
    data = [
        ":git_install_dir",
        "//cli:bazel-diff",
    ],
    visibility = ["//visibility:public"],
    deps = ["@bazel_tools//tools/bash/runfiles"],
)

# Exercises the hermetic git through exactly the operations serve's
# ProcessGitClient performs: clone, fetch --all --prune, targeted fetch of a
# SHA, rev-parse --verify <rev>^{commit}, and -c gc.auto=0 checkout --force.
sh_test(
    name = "git_smoke_test",
    size = "medium",
    srcs = ["git_smoke_test.sh"],
    args = ["$(rlocationpath :git_install_dir)"],
    data = [":git_install_dir"],
    deps = ["@bazel_tools//tools/bash/runfiles"],
)
