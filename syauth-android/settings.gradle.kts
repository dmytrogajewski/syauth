// Roadmap item S-015 — mirrors `~/sources/prrr/prrr-android/settings.gradle.kts`.
//
// pluginManagement and dependencyResolutionManagement blocks pin the same
// repository set that prrr-android proved out: google() + mavenCentral() for
// the AGP / Compose / JNA dep graph, plus gradlePluginPortal() for AGP itself.
// `FAIL_ON_PROJECT_REPOS` is mirrored so no per-module `repositories { ... }`
// block can silently drift the dep graph.

pluginManagement {
    repositories {
        google()
        mavenCentral()
        gradlePluginPortal()
    }
}

dependencyResolutionManagement {
    repositoriesMode.set(RepositoriesMode.FAIL_ON_PROJECT_REPOS)
    repositories {
        google()
        mavenCentral()
    }
}

rootProject.name = "syauth-android"
include(":app")
