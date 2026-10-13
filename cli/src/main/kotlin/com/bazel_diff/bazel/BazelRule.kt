package com.bazel_diff.bazel

import com.bazel_diff.hash.safePutBytes
import com.bazel_diff.hash.sha256
import com.google.devtools.build.lib.query2.proto.proto2api.Build

// Ignore generator_location when computing a target's hash since it is likely to change and does
// not
// affect a target's generated actions. Internally, Bazel also does this when computing a target's
// hash:
// https://github.com/bazelbuild/bazel/blob/6971b016f1e258e3bb567a0f9fe7a88ad565d8f2/src/main/java/com/google/devtools/build/lib/query2/query/output/SyntheticAttributeHashCalculator.java#L78-L81
private val DEFAULT_IGNORED_ATTRS = arrayOf("generator_location")

// Separator between a label and the checksum of the configuration it was analyzed in.
//
// A cquery graph is a graph of *configured* targets: one label can be analyzed in several
// configurations (a transition, an exec dependency, a `select()` that resolves differently), and
// those instances have different dependencies. Bazel identifies each dependency edge by
// `(label, configuration checksum)` in `configuredRuleInput`, which [encodeConfiguredRuleInput]
// folds into `label|checksum` (#359). [BazelQueryService] names configured rules and generated
// files the same way, so an edge resolves to the instance it points at rather than to whichever
// instance of the label came first: collapsing by label lost edges and, when an instance depended
// on the same label in another configuration, reported a cycle the configured graph does not have
// (Tinder/bazel-diff#512). The full encoded string is what gets mixed into a rule's digest; the
// bare label, via [decodeConfiguredRuleInputLabel], is what the hash file is keyed by.
const val CONFIGURED_RULE_INPUT_SEPARATOR: Char = '|'

fun encodeConfiguredRuleInput(input: Build.ConfiguredRuleInput): String {
  // Fall back to a bare label when the checksum is empty -- this matches the pre-#359 behaviour
  // for the known Bazel quirk where cquery's `transitions=lite` output sometimes omits the
  // configurationChecksum for an edge, and avoids appending a dangling separator.
  val checksum = input.configurationChecksum
  return if (checksum.isNullOrEmpty()) input.label else configuredTargetName(input.label, checksum)
}

/** Names a configured target by its label and the checksum of its configuration. */
fun configuredTargetName(label: String, checksum: String): String =
    "$label$CONFIGURED_RULE_INPUT_SEPARATOR$checksum"

fun decodeConfiguredRuleInputLabel(encoded: String): String {
  // No separator present -> the input is a bare label (non-cquery callers, empty-checksum
  // fallback, or //external:* synthetic inputs), so this is a no-op.
  return encoded.substringBefore(CONFIGURED_RULE_INPUT_SEPARATOR)
}

/**
 * The names in [names] that carry a configuration checksum, grouped by their bare label and sorted:
 * the fallback for an edge that names a configured instance absent from the graph. Empty for a
 * plain query, where every name is a bare label.
 */
fun configuredLabelInstances(names: Collection<String>): Map<String, List<String>> {
  val instances = HashMap<String, MutableList<String>>()
  for (name in names) {
    val label = decodeConfiguredRuleInputLabel(name)
    if (label != name) instances.getOrPut(label) { mutableListOf() }.add(name)
  }
  instances.values.forEach { it.sort() }
  return instances
}

class BazelRule(private val rule: Build.Rule) {
  fun digest(ignoredAttrs: Set<String>): ByteArray {
    return sha256 {
      safePutBytes(rule.ruleClassBytes.toByteArray())
      // The bare label: a configured instance's definition digest does not depend on which
      // configuration it was analyzed in, so a label hashed once keeps its pre-#512 digest.
      safePutBytes(decodeConfiguredRuleInputLabel(rule.name).toByteArray())
      safePutBytes(rule.skylarkEnvironmentHashCodeBytes.toByteArray())
      // Hash attributes in a canonical (name-sorted) order so a rule's digest is invariant to the
      // order Bazel happens to emit them in. Attribute names are unique within a rule (the proto
      // is generated from the rule's attribute map), so a name sort fully canonicalizes the set.
      // Mirrors target-determinator's `sortedAttributesForHashing` (commit d4b6125).
      for (attribute in rule.attributeList.sortedBy { it.name }) {
        if (!DEFAULT_IGNORED_ATTRS.contains(attribute.name) &&
            !ignoredAttrs.contains(attribute.name))
            safePutBytes(attribute.toByteArray())
      }
    }
  }

  fun ruleInputList(useCquery: Boolean, fineGrainedHashExternalRepos: Set<String>): List<String> {
    // Transformed //external:* synthetic inputs so that targets depending on external repos pick
    // up hash changes from //external:* synthetic targets (e.g. from bzlmod mod show_repo or
    // WORKSPACE //external:all-targets).
    val externalSyntheticInputs =
        rule.ruleInputList
            .map { ruleInput: String ->
              transformRuleInput(fineGrainedHashExternalRepos, ruleInput)
            }
            .filter { it.startsWith("//external:") }

    val inputs =
        if (useCquery) {
          // configuredRuleInputList already covers the non-fine-grained deps (with per-edge
          // configuration folded in), so only the //external:* synthetic inputs are added.
          rule.configuredRuleInputList.map { encodeConfiguredRuleInput(it) } +
              externalSyntheticInputs
        } else {
          rule.ruleInputList + externalSyntheticInputs
        }

    // Canonicalize: dedupe and sort so a target's digest is invariant to the order Bazel happens
    // to emit (configured) rule inputs in. This matters most under --useCquery, where the same dep
    // label can surface across multiple configurations and cquery does not guarantee a stable
    // emission order -- without canonicalization an unchanged target could hash differently
    // between two otherwise-identical graphs. RuleHasher mixes these bytes into the digest in list
    // order, so a deterministic order is what keeps the hash stable. Mirrors
    // target-determinator's `canonicalizeRuleInputs` (commit d4b6125).
    return inputs.distinct().sorted()
  }

  val name: String = rule.name

  /**
   * True for `package_group` targets. `bazel query` reports them under their own PACKAGE_GROUP
   * discriminator; [BazelQueryService] lowers them to synthetic rules with this rule class so they
   * participate in hashing (issue #441).
   */
  val isPackageGroup: Boolean
    get() = rule.ruleClass == "package_group"

  /**
   * The package_group labels referenced by this rule's `visibility` attribute. A visibility value
   * is either a package_group label or one of the special forms -- `//visibility:public`,
   * `//visibility:private`, or a package spec (`//foo:__pkg__` / `//foo:__subpackages__`) -- which
   * name no target of their own and are filtered out here.
   *
   * Visibility labels are "nodep" labels: `bazel query` never lists them in `rule_input`, so
   * [ruleInputList] cannot surface them and [com.bazel_diff.hash.RuleHasher] follows them
   * explicitly instead (issue #441).
   */
  fun visibilityPackageGroupLabels(): List<String> =
      rule.attributeList
          .firstOrNull { it.name == "visibility" }
          ?.stringListValueList
          .orEmpty()
          .filter {
            !it.startsWith("//visibility:") &&
                !it.endsWith(":__pkg__") &&
                !it.endsWith(":__subpackages__")
          }
          .distinct()
          .sorted()

  /**
   * Macro instantiation frames (`"<path>:<line>:<col>: <fn>"`) naming the BUILD/`.bzl` files that
   * produced this rule. Empty unless the query ran with `--proto:instantiation_stack`.
   */
  val instantiationStack: List<String>
    get() = rule.instantiationStackList

  /**
   * The values of this rule's `tags` attribute, or an empty set when the attribute is absent. Bazel
   * emits user-declared tags as a `STRING_LIST` attribute named `tags` in the query proto.
   * bazel-diff reads them to honour `--alwaysAffectedTags`: a target carrying one of those tags is
   * treated as always impacted, because it may read undeclared workspace state at execution time
   * (issue #401). This is the Bazel target *tag* `external`, which is unrelated to the external
   * *repository* handling behind `--excludeExternalTargets` / `--fineGrainedHashExternalRepos`.
   */
  fun tags(): Set<String> =
      rule.attributeList.firstOrNull { it.name == "tags" }?.stringListValueList?.toSet()
          ?: emptySet()

  private fun transformRuleInput(
      fineGrainedHashExternalRepos: Set<String>,
      ruleInput: String
  ): String {
    if (isNotMainRepo(ruleInput) &&
        ruleInput.startsWith("@") &&
        // Match on the full repo name (up to the `//` boundary), not a bare string
        // prefix: with hub-and-spoke repos (e.g. rules_python's `@pip` hub and
        // `@pip_<pkg>` spokes), a bare prefix check makes every spoke label look
        // like it belongs to the fine-grained hub, so it is never rewritten to its
        // `//external:<spoke>` seed and changes stop propagating.
        fineGrainedHashExternalRepos.none { ruleInput == it || ruleInput.startsWith("$it//") }) {
      val splitRule = ruleInput.split("//".toRegex()).dropLastWhile { it.isEmpty() }.toTypedArray()
      if (splitRule.size == 2) {
        var externalRule = splitRule[0]
        externalRule = externalRule.replaceFirst("@+".toRegex(), "")
        return String.format("//external:%s", externalRule)
      }
    }
    return ruleInput
  }

  private fun isNotMainRepo(ruleInput: String): Boolean {
    return !ruleInput.startsWith("//") &&
        !ruleInput.startsWith("@//") &&
        !ruleInput.startsWith("@@//")
  }
}
