# Linux benchmark image for the Firecracker snapshot harness.
#
# Runs the cold-vs-warm analysis-time benchmark and the local-driver
# orchestrator on a generated large-Bazel-project, inside real Linux (the CI
# target OS). Does NOT run Firecracker itself — that needs /dev/kvm, which is
# unavailable in Docker-for-Mac. See README.md.
#
# The JDK is for the nested Bazel server the benchmark drives, not for
# bazel-diff, which is a self-contained static binary.
FROM eclipse-temurin:21-jdk

ARG BAZELISK_ARCH=arm64
ARG BAZELISK_VERSION=v1.20.0

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        git python3 bash curl ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && curl -fsSL -o /usr/local/bin/bazelisk \
        "https://github.com/bazelbuild/bazelisk/releases/download/${BAZELISK_VERSION}/bazelisk-linux-${BAZELISK_ARCH}" \
    && chmod +x /usr/local/bin/bazelisk \
    && ln -sf /usr/local/bin/bazelisk /usr/local/bin/bazel

# bazel-diff: the statically linked musl binary for this architecture, on PATH.
COPY bazel-diff /usr/local/bin/bazel-diff
RUN chmod +x /usr/local/bin/bazel-diff

# The Go orchestrator (linux binary) + the bench scripts.
COPY bazel-diff-snap /usr/local/bin/bazel-diff-snap
COPY gen_project.py bench.py run_in_container.sh /opt/bench/
RUN chmod +x /usr/local/bin/bazel-diff-snap /opt/bench/run_in_container.sh

WORKDIR /work
ENTRYPOINT ["/opt/bench/run_in_container.sh"]
