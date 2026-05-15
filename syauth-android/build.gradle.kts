// Roadmap item S-015 — mirrors `~/sources/prrr/prrr-android/build.gradle.kts`.
// Top-level build file declaring the two plugins every Android+Kotlin project
// needs. Plugin versions are pinned to the exact set prrr-android shipped
// (AGP 8.2.2, Kotlin 1.9.22) so the Compose compiler extension version
// (1.5.8, pinned in `app/build.gradle.kts`) stays binary-compatible.

plugins {
    id("com.android.application") version "8.2.2" apply false
    id("org.jetbrains.kotlin.android") version "1.9.22" apply false
}
