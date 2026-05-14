# syauth-android (placeholder)

This directory is the Gradle placeholder reserved by roadmap item **S-001** for
the Android companion app. Real Gradle wiring (Compose UI, JNA loader, Compose
build files, JNI ABI list) lands with **S-015** in the roadmap.

For the bootstrap commit it contains only:

- `settings.gradle.kts` — Gradle root with a single `:app` include.
- `app/` — placeholder for the application module (kept empty by an in-tree
  `.gitkeep`; the real `build.gradle.kts` arrives in S-015).

The structure mirrors `~/sources/prrr/prrr-android/` so that S-015 can copy
build files verbatim and adjust the package name to `com.sy.syauth.android`.
