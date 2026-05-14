// Placeholder Gradle settings for the syauth Android companion app.
//
// Roadmap item S-015 fills this in by mirroring `~/sources/prrr/prrr-android/
// settings.gradle.kts`. S-001 ships only the placeholder so that the directory
// exists, the DoD checkbox can be ticked, and downstream work has a stable
// path to bind to. The `pluginManagement` and `dependencyResolutionManagement`
// blocks land with S-015.

rootProject.name = "syauth-android"
include(":app")
