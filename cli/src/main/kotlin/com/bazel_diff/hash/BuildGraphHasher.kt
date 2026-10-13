package com.bazel_diff.hash

import com.bazel_diff.bazel.BazelClient
import com.bazel_diff.bazel.BazelModService
import com.bazel_diff.bazel.BazelRule
import com.bazel_diff.bazel.BazelSourceFileTarget
import com.bazel_diff.bazel.BazelTarget
import com.bazel_diff.bazel.configuredLabelInstances
import com.bazel_diff.bazel.decodeConfiguredRuleInputLabel
import com.bazel_diff.extensions.toHexString
import com.bazel_diff.log.Logger
import java.nio.file.Path
import java.util.Calendar
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.ConcurrentMap
import java.util.concurrent.atomic.AtomicReference
import java.util.stream.Collectors
import kotlin.io.path.readBytes
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.async
import kotlinx.coroutines.runBlocking
import org.koin.core.component.KoinComponent
import org.koin.core.component.inject

/**
 * Mutable per-run phase timings, filled in by [BuildGraphHasher.hashAllBazelTargetsAndSourcefiles]
 * when the caller passes one. The phases run sequentially, in field order; [targetHashMillis] also
 * covers deriving the seed and per-package `.bzl` seeds that target hashing consumes.
 */
class HasherPhaseTimings {
  var bazelQueryMillis: Long = 0
  var sourceHashMillis: Long = 0
  var targetHashMillis: Long = 0
}

class BuildGraphHasher(private val bazelClient: BazelClient) : KoinComponent {
  private val targetHasher: TargetHasher by inject()
  private val sourceFileHasher: SourceFileHasher by inject()
  private val bazelModService: BazelModService by inject()
  private val logger: Logger by inject()

  fun hashAllBazelTargetsAndSourcefiles(
      seedFilepaths: Set<Path> = emptySet(),
      ignoredAttrs: Set<String> = emptySet(),
      modifiedFilepaths: Set<Path> = emptySet(),
      timings: HasherPhaseTimings? = null
  ): Map<String, TargetHash> {
    val hashInvocationContext = HashInvocationContext()
    val (sourceDigests, allTargets) =
        runBlocking {
          val queryStartNanos = System.nanoTime()
          val targetsTask = async(Dispatchers.IO) { bazelClient.queryAllTargets() }
          val allTargets = targetsTask.await()
          timings?.bazelQueryMillis = (System.nanoTime() - queryStartNanos) / 1_000_000
          val sourceTargets =
              allTargets
                  .filter { it is BazelTarget.SourceFile }
                  .map { it as BazelTarget.SourceFile }

          val sourceDigestsFuture =
              async(Dispatchers.IO) {
                val sourceHashDurationEpoch = Calendar.getInstance().getTimeInMillis()
                val sourceFileTargets = hashSourcefiles(sourceTargets, modifiedFilepaths)
                val sourceHashDuration =
                    Calendar.getInstance().getTimeInMillis() - sourceHashDurationEpoch
                timings?.sourceHashMillis = sourceHashDuration
                logger.i { "Source file hashes calculated in $sourceHashDuration" }
                sourceFileTargets
              }

          Pair(sourceDigestsFuture.await(), allTargets)
        }
    val targetHashStartNanos = System.nanoTime()
    val seedForFilepaths = runBlocking(Dispatchers.IO) { createSeedForFilepaths(seedFilepaths) }
    // With macro instantiation stacks (`--proto:instantiation_stack`), RuleHasher.ruleBzlSeed
    // attributes each `.bzl` to the rules a macro produced, so the coarse package seed (and its
    // source-file over-invalidation) is dropped. Fall back to it when stacks are absent (#365).
    val hasInstantiationStacks =
        allTargets.any { it is BazelTarget.Rule && it.rule.instantiationStack.isNotEmpty() }
    val packageBzlSeeds =
        if (hasInstantiationStacks) emptyMap()
        else createPackageBzlSeeds(allTargets, modifiedFilepaths)
    val result =
        hashAllTargets(
            seedForFilepaths,
            packageBzlSeeds,
            sourceDigests,
            allTargets,
            ignoredAttrs,
            modifiedFilepaths,
            hashInvocationContext)
    timings?.targetHashMillis = (System.nanoTime() - targetHashStartNanos) / 1_000_000
    return result
  }

  private fun hashSourcefiles(
      targets: List<BazelTarget.SourceFile>,
      modifiedFilepaths: Set<Path>
  ): ConcurrentMap<String, ByteArray> {
    val exception = AtomicReference<Exception?>(null)
    val result: ConcurrentMap<String, ByteArray> =
        targets
            .parallelStream()
            .map { sourceFile: BazelTarget.SourceFile ->
              val seed = sha256 {
                safePutBytes(sourceFile.name.toByteArray())
                for (subinclude in sourceFile.subincludeList) {
                  safePutBytes(subinclude.toByteArray())
                }
              }
              try {
                val sourceFileTarget = BazelSourceFileTarget(sourceFile.name, seed)
                Pair(
                    sourceFileTarget.name,
                    sourceFileHasher.digest(sourceFileTarget, modifiedFilepaths))
              } catch (e: Exception) {
                exception.set(e)
                null
              }
            }
            .filter { pair -> pair != null }
            .collect(
                Collectors.toConcurrentMap(
                    { pair -> pair!!.first },
                    { pair -> pair!!.second },
                ))

    exception.get()?.let { throw it }
    return result
  }

  private fun hashAllTargets(
      seedHash: ByteArray,
      packageBzlSeeds: Map<String, ByteArray>,
      sourceDigests: ConcurrentMap<String, ByteArray>,
      allTargets: List<BazelTarget>,
      ignoredAttrs: Set<String>,
      modifiedFilepaths: Set<Path>,
      hashInvocationContext: HashInvocationContext
  ): Map<String, TargetHash> {
    val ruleHashes: ConcurrentMap<String, TargetDigest> = ConcurrentHashMap()
    val targetToRule: MutableMap<String, BazelRule> = HashMap()
    traverseGraph(allTargets, targetToRule)

    val digests =
        allTargets
            .parallelStream()
            .map { target: BazelTarget ->
              Triple(
                  target.name,
                  target.javaClass.name.substringAfterLast('$'),
                  targetHasher.digest(
                      target,
                      targetToRule,
                      sourceDigests,
                      ruleHashes,
                      seedHash,
                      packageBzlSeeds,
                      ignoredAttrs,
                      modifiedFilepaths,
                      hashInvocationContext))
            }
            .collect(Collectors.toList())
    return aggregateByLabel(digests)
  }

  /**
   * Folds per-name digests into the per-label hashes the hash file is keyed by. Under cquery a
   * target's name is its configured identity (`label|checksum`, see
   * [com.bazel_diff.bazel.CONFIGURED_RULE_INPUT_SEPARATOR]), and one label can be hashed in several
   * configurations. A label hashed once keeps its digest as is, so hash files do not change where
   * nothing was collapsed. A label with several instances hashes all of them, in name order, so a
   * change reaching any instance changes the label, and its dependency edges are the union of its
   * instances', as labels.
   */
  private fun aggregateByLabel(
      digests: List<Triple<String, String, TargetDigest>>
  ): Map<String, TargetHash> {
    val result = HashMap<String, TargetHash>(digests.size * 4 / 3 + 1)
    for ((label, instances) in digests.groupBy { decodeConfiguredRuleInputLabel(it.first) }) {
      val sorted = if (instances.size == 1) instances else instances.sortedBy { it.first }
      val (_, type, first) = sorted[0]
      val overall: ByteArray
      val direct: ByteArray
      if (sorted.size == 1) {
        overall = first.overallDigest
        direct = first.directDigest
      } else {
        overall = sha256 { for ((_, _, digest) in sorted) safePutBytes(digest.overallDigest) }
        direct = sha256 { for ((_, _, digest) in sorted) safePutBytes(digest.directDigest) }
      }
      val deps =
          if (first.deps == null) null
          else
              sorted
                  .flatMap { it.third.deps.orEmpty() }
                  .map { decodeConfiguredRuleInputLabel(it) }
                  .distinct()
      result[label] = TargetHash(type, overall.toHexString(), direct.toHexString(), deps)
    }
    return result
  }

  /**
   * Maps every target name to the rule that produces it: a rule to itself, a generated file to its
   * generating rule. Under cquery a generated file names its generating rule in the file's own
   * configuration; when that instance is not in the graph, the file resolves to another instance of
   * the same label.
   */
  private fun traverseGraph(
      allTargets: List<BazelTarget>,
      targetToRule: MutableMap<String, BazelRule>
  ) {
    for (target in allTargets) {
      if (target is BazelTarget.Rule) targetToRule[target.name] = target.rule
    }
    var labelInstances: Map<String, List<String>>? = null
    for (target in allTargets) {
      if (target !is BazelTarget.GeneratedFile) continue
      val rule =
          targetToRule[target.generatingRuleName]
              ?: run {
                val instances =
                    labelInstances
                        ?: configuredLabelInstances(targetToRule.keys).also { labelInstances = it }
                instances[decodeConfiguredRuleInputLabel(target.generatingRuleName)]
                    ?.firstOrNull()
                    ?.let { targetToRule[it] }
              }
              ?: throw RuntimeException("Not possible to traverse the build graph")
      targetToRule[target.name] = rule
    }
  }

  private suspend fun createSeedForFilepaths(seedFilepaths: Set<Path>): ByteArray {
    return sha256 {
      // Include seed filepaths in hash
      for (path in seedFilepaths) {
        putBytes(path.readBytes())
      }
    }
  }

  /**
   * Builds a per-package seed-hash contribution from the contents of every `.bzl` (and `.scl`) file
   * that package's BUILD file loads, keyed by package label (e.g. `//pkg`, `//` for the root
   * package).
   *
   * Background: Bazel pre-7 populated [Build.Rule.skylark_environment_hash_code] in the query
   * proto, so any change to a `.bzl` file loaded by a rule's BUILD file naturally bubbled into that
   * rule's hash. Bazel 7+ leaves that field empty, which is the root cause of issues
   * [#259](https://github.com/Tinder/bazel-diff/issues/259) and
   * [#227](https://github.com/Tinder/bazel-diff/issues/227): editing a macro body (e.g. adding
   * `print()`) no longer invalidated any caller because the emitted rule attrs were identical.
   *
   * Fix: each package's BUILD `SourceFile` target carries a `subincludeList` (the
   * `Build.SourceFile.subinclude` proto field) listing every `.bzl` that BUILD loaded. We
   * softDigest each main-repo `.bzl` and roll the digests for a given package into a seed
   * attributed to that package. Callers then mix only their own package's seed into each target's
   * hash, so editing a `.bzl` re-hashes exactly the targets in packages that `load()` it -- not
   * every target in the workspace ([#365](https://github.com/Tinder/bazel-diff/issues/365)). A
   * package that loads no tracked `.bzl` has no entry here, so nothing is mixed in and its targets
   * stay byte-for-byte stable.
   *
   * External-repo `.bzl` files (`@repo//...`, `@@canonical//...`) are skipped because
   * [SourceFileHasher.softDigest] returns null for non-main-repo labels, which keeps the seed
   * stable across BCR fetches that don't actually change repo contents.
   */
  private fun createPackageBzlSeeds(
      allTargets: List<BazelTarget>,
      modifiedFilepaths: Set<Path>
  ): Map<String, ByteArray> {
    // Union the loaded `.bzl` labels per package. Every source file in a package shares the same
    // BUILD, but the `subinclude` field is populated on the BUILD's SourceFile target; group by
    // package so we don't depend on which source file carries it.
    val packageToLabels = sortedMapOf<String, java.util.SortedSet<String>>()
    for (target in allTargets) {
      if (target !is BazelTarget.SourceFile || target.subincludeList.isEmpty()) continue
      packageToLabels
          .getOrPut(labelToPackage(target.sourceFileName)) { sortedSetOf() }
          .addAll(target.subincludeList)
    }

    val result = mutableMapOf<String, ByteArray>()
    for ((pkg, labels) in packageToLabels) {
      val tracked = mutableListOf<Pair<String, ByteArray>>()
      for (label in labels) {
        val digest =
            sourceFileHasher.softDigest(
                BazelSourceFileTarget(label, ByteArray(0)), modifiedFilepaths)
        if (digest != null) tracked += label to digest
      }
      if (tracked.isEmpty()) continue
      result[pkg] = sha256 {
        for ((label, digest) in tracked) {
          safePutBytes(label.toByteArray())
          safePutBytes(digest)
        }
      }
    }
    return result
  }
}

/**
 * Returns the package portion of a Bazel label, i.e. everything before the target separator `:`.
 * `//pkg:a` -> `//pkg`, `//:logo` -> `//`, `@@repo//pkg:a` -> `@@repo//pkg`. Labels without a `:`
 * (not expected from `bazel query` output) are returned unchanged.
 */
fun labelToPackage(label: String): String {
  val colon = label.lastIndexOf(':')
  return if (colon >= 0) label.substring(0, colon) else label
}
