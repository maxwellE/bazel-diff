package com.bazel_diff.hash

import com.bazel_diff.bazel.BazelRule
import com.bazel_diff.bazel.configuredLabelInstances
import java.util.concurrent.ConcurrentHashMap

/**
 * Mutable state that may be shared while one build graph is hashed.
 *
 * Hashers are application singletons and may outlive a workspace checkout. Revision-derived data
 * must therefore live here instead of on a hasher. The concurrent map preserves digest
 * deduplication while targets are hashed in parallel.
 */
class HashInvocationContext {
  private val bzlDigests = ConcurrentHashMap<String, ByteArray>()
  @Volatile private var labelInstances: Map<String, List<String>>? = null

  fun bzlDigest(path: String, calculate: () -> ByteArray?): ByteArray? {
    val digest = bzlDigests.computeIfAbsent(path) { calculate() ?: MISSING_DIGEST }
    return digest.takeUnless { it === MISSING_DIGEST }
  }

  /**
   * [configuredLabelInstances] of the graph's rule names, built on first use since most graphs
   * never need it, and shared by the parallel hashers of one invocation.
   */
  fun labelInstances(rules: Map<String, BazelRule>): Map<String, List<String>> {
    labelInstances?.let {
      return it
    }
    synchronized(this) {
      return labelInstances ?: configuredLabelInstances(rules.keys).also { labelInstances = it }
    }
  }

  private companion object {
    val MISSING_DIGEST = ByteArray(0)
  }
}
