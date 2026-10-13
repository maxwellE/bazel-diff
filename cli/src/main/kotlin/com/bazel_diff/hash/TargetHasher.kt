package com.bazel_diff.hash

import com.bazel_diff.bazel.BazelRule
import com.bazel_diff.bazel.BazelTarget
import java.nio.file.Path
import java.util.concurrent.ConcurrentMap
import org.koin.core.component.KoinComponent
import org.koin.core.component.inject

class TargetHasher : KoinComponent {
  private val ruleHasher: RuleHasher by inject()

  fun digest(
      target: BazelTarget,
      allRulesMap: Map<String, BazelRule>,
      sourceDigests: ConcurrentMap<String, ByteArray>,
      ruleHashes: ConcurrentMap<String, TargetDigest>,
      seedHash: ByteArray?,
      packageBzlSeeds: Map<String, ByteArray>,
      ignoredAttrs: Set<String>,
      modifiedFilepaths: Set<Path>,
      hashInvocationContext: HashInvocationContext
  ): TargetDigest {
    return when (target) {
      is BazelTarget.GeneratedFile -> {
        // The file's own entry in allRulesMap is its generating rule as BuildGraphHasher resolved
        // it, which under cquery may be another configured instance of the rule the file names.
        val generatingRule =
            allRulesMap[target.generatingRuleName]
                ?: allRulesMap[target.name]
                ?: throw RuntimeException("Unexpected generating rule ${target.generatingRuleName}")
        var digest =
            ruleHashes[generatingRule.name]
                ?: ruleHasher.digest(
                    generatingRule,
                    allRulesMap,
                    ruleHashes,
                    sourceDigests,
                    seedHash,
                    packageBzlSeeds,
                    depPath = null,
                    ignoredAttrs,
                    modifiedFilepaths,
                    hashInvocationContext)

        // Add the generating rule name as a dep of the generated file.
        digest = digest.clone(newDeps = listOf(generatingRule.name))
        digest
      }
      is BazelTarget.Rule -> {
        ruleHasher.digest(
            target.rule,
            allRulesMap,
            ruleHashes,
            sourceDigests,
            seedHash,
            packageBzlSeeds,
            depPath = null,
            ignoredAttrs,
            modifiedFilepaths,
            hashInvocationContext)
      }
      is BazelTarget.SourceFile -> {
        // Mix in the `.bzl` seed for this source file's own package only, so a macro edit in
        // another package leaves this file's hash untouched (issue #365).
        val digest = sha256 {
          safePutBytes(sourceDigests[target.sourceFileName])
          safePutBytes(seedHash)
          safePutBytes(packageBzlSeeds[labelToPackage(target.sourceFileName)])
        }
        TargetDigest(digest, digest)
      }
    }
  }
}
