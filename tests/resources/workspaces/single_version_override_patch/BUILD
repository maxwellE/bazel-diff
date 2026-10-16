# Reads a file that only exists in its patched form: the patch under //patches
# rewrites greeting.txt inside @patched_dep, so this target's output follows the
# patch file, not just the module version.
genrule(
    name = "consumer",
    srcs = ["@patched_dep//:greeting.txt"],
    outs = ["consumer.out"],
    cmd = "cat $(SRCS) > $@",
)

# Does not depend on @patched_dep; a patch edit must leave it alone.
genrule(
    name = "control",
    outs = ["control.out"],
    cmd = "echo control > $@",
)
