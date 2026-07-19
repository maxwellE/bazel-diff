"""Bzlmod extensions for repositories used only when building bazel-diff itself.

Contains:
  * non_module_repositories: formatting tools (e.g. ktfmt).
  * hermetic_git: source archives for the hermetic `git` built by //tools/git.
"""

load("@bazel_tools//tools/build_defs/repo:http.bzl", "http_archive", "http_jar")

def _non_module_repositories_impl(_module_ctx):
    # Fetch ktfmt JAR from Maven Central
    # This replaces the removed fetch_ktfmt() from aspect_rules_lint v1.x
    http_jar(
        name = "ktfmt",
        integrity = "sha256-l/x/vRlNAan6RdgUfAVSQDAD1VusSridhNe7TV4/SN4=",
        url = "https://repo1.maven.org/maven2/com/facebook/ktfmt/0.46/ktfmt-0.46-jar-with-dependencies.jar",
    )

non_module_repositories = module_extension(_non_module_repositories_impl)

# git release built by //tools/git:git. The kernel.org dist tarball (not a
# GitHub auto-generated archive) has a stable checksum and ships the `version`
# file GIT-VERSION-GEN reads, so the binary reports the exact pinned version.
_GIT_VERSION = "2.55.0"
_GIT_SHA256 = "457fdb04dc8728e007d4688695e6912e6f680727920f2a40bf11eacc17505357"

# zlib is git's only mandatory library dependency. Built from source (see
# //tools/git:zlib) rather than taken from the bzlmod `zlib` module because
# git's Makefile links with `-lz`, which requires a library named libz.a; the
# bzlmod module's cc_library produces libzlib.a. Same tarball the Bazel Central
# Registry pins for its zlib module.
_ZLIB_VERSION = "1.3.1"
_ZLIB_SHA256 = "9a93b2b7dfdac77ceba5a558a580e74667dd6fede4585b91eefb60f03b72df23"

def _hermetic_git_impl(_module_ctx):
    http_archive(
        name = "git_source",
        build_file = Label("//tools/git:git_source.BUILD"),
        sha256 = _GIT_SHA256,
        strip_prefix = "git-" + _GIT_VERSION,
        urls = [
            "https://mirrors.edge.kernel.org/pub/software/scm/git/git-%s.tar.xz" % _GIT_VERSION,
            "https://www.kernel.org/pub/software/scm/git/git-%s.tar.xz" % _GIT_VERSION,
        ],
    )
    http_archive(
        name = "zlib_source",
        build_file = Label("//tools/git:zlib_source.BUILD"),
        sha256 = _ZLIB_SHA256,
        strip_prefix = "zlib-" + _ZLIB_VERSION,
        urls = [
            "https://github.com/madler/zlib/releases/download/v%s/zlib-%s.tar.gz" %
            (_ZLIB_VERSION, _ZLIB_VERSION),
        ],
    )

hermetic_git = module_extension(
    _hermetic_git_impl,
    doc = "Fetches the source archives for the hermetic git built by //tools/git " +
          "(see tools/git/BUILD), which the serve capability can use via " +
          "`bazel run //tools/git:serve` instead of a system git.",
)
