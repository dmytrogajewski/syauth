# syauth-android

Android companion app for the syauth phone-as-key unlock protocol.

## Status

| Roadmap item | What landed                                                             |
|--------------|-------------------------------------------------------------------------|
| S-001        | Gradle placeholder (settings.gradle.kts, app/.gitkeep).                 |
| **S-015**    | Real Gradle + Compose scaffold. Single hello-world screen consuming the |
|              | `syauth_mobile.aar` (S-014) and rendering the OOB code for a fixture    |
|              | bond key. **No Bluetooth code yet** — proves the toolchain end-to-end.  |

The structure mirrors `~/sources/prrr/prrr-android/` line-for-line; see
[`specs/journeys/JOURNEY-S-015-android-scaffold.md`](../specs/journeys/JOURNEY-S-015-android-scaffold.md)
for the per-file delta table.

## Build

S-015 ships **debug-only**. Release signing + R8 land later.

Prerequisites on the build host:

- JDK 17+ (AGP 8.2.x requirement). The `JAVA_HOME` env var must point at it.
- Android SDK with `cmdline-tools;latest` and `platforms;android-34`.
- The `syauth_mobile.aar` produced by `make android-aar` from the workspace
  root. On hosts without the Android NDK, `make android-aar-dry-run` verifies
  the pipeline but does not produce the artifact.

From this directory:

```bash
./gradlew :app:assembleDebug
```

The resulting APK lands at `app/build/outputs/apk/debug/app-debug.apk`. The
DoD asserts the size is under 10 MB.

## Test

The instrumented `HelloWorldTest` launches `MainActivity` on a connected
device or emulator and asserts the OOB string rendered:

```bash
# From the workspace root:
make android-test
```

`make android-test` skips cleanly with an actionable message when either:

- `crates/syauth-mobile/target/syauth_mobile.aar` does not exist
  (run `make android-aar` on an NDK-equipped host first), or
- no emulator / device is connected over `adb`.

## Versions (mirrors prrr-android)

| Component                    | Version           | Source line in prrr-android        |
|------------------------------|-------------------|------------------------------------|
| AGP                          | 8.2.2             | `build.gradle.kts` plugins block.  |
| Kotlin                       | 1.9.22            | `build.gradle.kts` plugins block.  |
| compileSdk                   | 34                | `app/build.gradle.kts` line 22.    |
| minSdk                       | 26                | `app/build.gradle.kts` line 30.    |
| targetSdk                    | 34                | `app/build.gradle.kts` line 31.    |
| JVM target                   | 17                | `app/build.gradle.kts` lines 70-75 |
| Compose compiler ext         | 1.5.8             | `app/build.gradle.kts` line 83.    |
| Compose BOM                  | 2024.02.00        | `app/build.gradle.kts` line 111.   |
| JNA                          | 5.14.0            | `app/build.gradle.kts` line 96.    |
| Gradle wrapper               | 9.5.0-milestone-3 | `gradle/wrapper/...properties`.    |

The Gradle wrapper (`gradle/wrapper/gradle-wrapper.jar`, `gradlew`,
`gradlew.bat`) is copied verbatim from prrr-android.
