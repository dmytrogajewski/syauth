# JOURNEY-S-015: Android scaffold — Gradle + Compose + hello-world consuming the AAR

<!-- Authored per .agents/skills/journey/SKILL.md template. -->

## Roadmap Link
- Source roadmap: [specs/syauth/ROADMAP.md](../syauth/ROADMAP.md) — item **S-015**.
- Feature: bootstrap `syauth-android/` as a real Gradle + Kotlin + Compose project that consumes the `syauth_mobile.aar` produced by S-014 and renders the OOB code for a fixed bond key. No Bluetooth code yet — this step proves end-to-end that the toolchain (Gradle → Compose → JNA → UniFFI → Rust) is wired correctly.

## 1. Journey

When **the Android app developer (Sam) bringing up `syauth-android/` for the first time after S-014 shipped the `syauth_mobile.aar`** I want to **drop the AAR into the Android Gradle project that mirrors `~/sources/prrr/prrr-android/` line-for-line, run `make android-test`, and see a single Compose screen render `OOB: <four emoji words>`** so I can **prove the Rust→UniFFI→Kotlin pipeline works end-to-end on a real (or emulated) device before touching Bluetooth (S-016) or BiometricPrompt (S-017)**.

## 2. CJM

S-015 is the seam between the Rust protocol core (S-002..S-007, S-014) and the Android UI layers (S-016..S-018). Until this step ships, the AAR built by `make android-aar` is just a zipped `.so` — nobody on the team has actually called into it from Kotlin. S-015's role is to be the first byte that crosses the JNA boundary on a real device, and it does so with the **smallest** possible payload: a single Compose screen that calls `oobCodeForBond(ByteArray(32) { it.toByte() })` and renders the result as a `Text("OOB: <words.joinToString(\" \")>")`. Nothing else.

Two design forces dominate the step:

1. **Mirror prrr-android.** The orchestrator brief is explicit and the maintenance argument is strong: prrr-android already shipped through the same pipeline (Compose + UniFFI + JNA) and any drift creates a maintenance dual that future agents have to keep in sync by hand. We copy the Gradle wiring (AGP 8.2.2, Kotlin 1.9.22, JVM 17, minSdk 26 / targetSdk 34 / compileSdk 34, Compose BOM 2024.02.00, kotlinCompilerExtensionVersion 1.5.8, JNA 5.14.0). The deltas are limited to namespace + AAR path + the test scope (drop CameraX, ZXing, security-crypto, lifecycle since S-015 doesn't need them; keep activity-compose + compose-bom + material3 + JNA).
2. **No hand-written JNI.** The DoD line "No hand-written JNI in the codebase. Every Rust call goes through the UniFFI-generated Kotlin." is non-negotiable. The Kotlin call site says `uniffi.syauth_mobile.oobCodeForBond(...)`. A `grep -rn 'external fun' syauth-android/` MUST return zero hits — UniFFI generates every JNA `Library` interface, every `Pointer` shuffling, every `external fun`. We never write them by hand.

A third constraint is operational: most developer hosts (and this CI host) have **no Android SDK**, **no NDK**, and **no emulator**. The DoD asks for a `make android-test` target that runs `./gradlew :app:connectedAndroidTest` against a headless emulator. We honor this by making the Makefile target:

- Skip cleanly if the AAR is missing (S-014's NDK-equipped CI builds it; this CI runs without an NDK).
- Skip cleanly if no `adb` device is connected.
- Print actionable messages so a developer with an SDK + emulator can run the same command without surprises.

### Phase 1: Mirror the Gradle wiring

**User Intent:** Sam wants the Gradle project to build cleanly on a host with the Android SDK installed, with no version drift from the proven prrr-android pipeline.

**Actions:** Sam runs `cd syauth-android && ./gradlew :app:assembleDebug` on a host with `ANDROID_SDK_ROOT` set and the AAR already produced.

**Pain / Risk:**
- AGP / Kotlin / Compose compiler extension versions are tightly coupled. Picking a Compose compiler extension version that doesn't match Kotlin 1.9.22 fails the build with an opaque error. Mitigation: copy `kotlinCompilerExtensionVersion = "1.5.8"` verbatim from prrr-android.
- Gradle wrapper version drift between prrr-android and syauth-android could surface as plugin-incompatibility errors. Mitigation: copy the wrapper jar + properties verbatim from prrr-android, pinning Gradle 9.5.0-milestone-3.
- The AAR includes a JNA-loaded `.so` per ABI; if the app declares an `abiFilters` that excludes the host emulator's ABI, the test fails with `UnsatisfiedLinkError`. Mitigation: do not set `abiFilters` — let the AAR's `jni/<abi>/` set the union.
- JVM 17 is a hard requirement for AGP 8.2.x. On hosts with JDK 11 only, `./gradlew assembleDebug` fails. Mitigation: document the requirement in the journey; mirror prrr-android's `compileOptions { sourceCompatibility = VERSION_17 ; targetCompatibility = VERSION_17 }`.

**Success Signal:** `./gradlew :app:assembleDebug` on an SDK-equipped host produces `app/build/outputs/apk/debug/app-debug.apk` whose size is under 10 MB.

### Phase 2: Compose screen renders the OOB call result

**User Intent:** When the app launches on the emulator, Sam wants to see `OOB: <emoji> <emoji> <emoji> <emoji>` in a single `Text` composable as proof that the Rust→JNA→Compose pipeline actually executed.

**Actions:** Sam runs `./gradlew :app:installDebug && adb shell am start -n com.sy.syauth.android/.MainActivity`.

**Pain / Risk:**
- Calling `oobCodeForBond` on the main thread blocks the UI thread briefly. The function is pure CPU (HKDF + table lookup, microseconds on ARMv8); we accept the synchronous call. Mitigation: document the cost in the source; the bond_key is a fixed `ByteArray(32) { it.toByte() }` so the call is deterministic.
- UniFFI bindings throw `MobileException` subclasses (Kotlin checked exceptions) on every error variant. For a fixed valid 32-byte input, no error path is reachable. Mitigation: wrap the call in a `runCatching { ... }.getOrElse { listOf("ERR: ${it.message ?: "unknown"}") }` so a binding regression still renders a visible string (the test would catch it with `assertIsDisplayed()` plus a regex check).
- The Compose `setContent { Surface { Text("OOB: $words") } }` requires Material3 + activity-compose + a Compose BOM that matches Kotlin 1.9.22. Mitigation: copy the prrr-android Compose BOM (2024.02.00) and material3 dep verbatim.
- `MaterialTheme` from prrr-android pulls in PrrrColorScheme; we keep the theme to a single `MaterialTheme { ... }` with default dark colors to avoid a 3-file color/type/theme expansion in S-015. The full theme expansion can land with S-016 if a designer asks.

**Success Signal:** Launching the app on an emulator displays `OOB: 🐱 🐶 🐭 🐰` (or whatever the four words are for the fixture key) at the top of the screen.

### Phase 3: Instrumented test asserts the render

**User Intent:** Sam wants CI (when it gets an emulator runner) to assert "the app launches AND the OOB string actually rendered" — proving the Rust call really executed, not just that the activity didn't crash.

**Actions:** `make android-test` from the repo root, which delegates to `cd syauth-android && ./gradlew :app:connectedAndroidTest`.

**Pain / Risk:**
- `androidx.compose.ui.test.junit4.createAndroidComposeRule<MainActivity>()` requires the test-junit4 artifact AND a `debugImplementation("androidx.compose.ui:ui-test-manifest")` AndroidManifest hook. Without the latter, the test runner fails with `ActivityNotFoundException`. Mitigation: copy prrr-android's `debugImplementation(...)` for `ui-test-manifest`.
- The instrumented test runner does not run the JVM-side `Theme.kt` `SideEffect { window.statusBarColor = ... }` block correctly when the activity was just created in the test rule; if the SideEffect throws on null window, the test fails. Mitigation: keep the theme minimal — no SideEffect — so the test rule constructs MainActivity without surprises.
- An emulator that doesn't have the right ABI (e.g., a Wear OS image) fails the JNA load. Mitigation: the AAR's `jni/{arm64-v8a,armeabi-v7a,x86_64,x86}/` covers every emulator image AOSP ships; we make no `abiFilters` constraint.
- The connected-test target hangs forever if no `adb` device is connected. Mitigation: `make android-test` first runs `adb devices | grep -E 'device$'` and exits 0 with a printed skip message if nothing is connected.

**Success Signal:** `make android-test` on an SDK + emulator host exits 0 with `BUILD SUCCESSFUL` and 1 instrumented test passed. On this CI host (no SDK, no emulator), it exits 0 with `(no Android emulator connected — skipping)`.

### Phase 4: APK size budget enforcement

**User Intent:** Sam wants to know if the hello-world APK is unexpectedly fat (which would mean the AAR is shipping the wrong `.so` symbols or the dep graph is pulling in CameraX/ZXing/etc).

**Actions:** `./gradlew :app:assembleDebug` produces `app/build/outputs/apk/debug/app-debug.apk`. `make android-test` greps the size and asserts `< 10 MB`.

**Pain / Risk:**
- The AAR alone is large (~3-5 MB depending on which ABIs the NDK build included); JNA adds ~1.5 MB; Compose runtime + material3 adds ~2 MB. Total is comfortably under 10 MB if we keep the dep set minimal. Mitigation: drop every dependency that S-015 doesn't need (CameraX, ZXing, security-crypto, kotlinx-coroutines-android — these all land in S-016+).
- A debug APK is *uncompressed*; switching to release with R8 would shrink things further, but the DoD asks about the debug APK. Mitigation: explicit `:assembleDebug` in the target, no R8.

**Success Signal:** `du -h app/build/outputs/apk/debug/app-debug.apk` reports a value < 10 MB.

### Phase 5: Robust failure on missing AAR

**User Intent:** Sam, on a host without the NDK (and therefore no `crates/syauth-mobile/target/syauth_mobile.aar`), runs `make android-test` and gets a clear, actionable message instead of a Gradle stacktrace.

**Actions:** `make android-test` on a host where `make android-aar` has never been run.

**Pain / Risk:**
- `implementation(files("../crates/syauth-mobile/target/syauth_mobile.aar"))` resolves at configuration time only if the file exists. If it's missing, Gradle still completes configuration (treating it as an empty file dep) but the linker fails at task time with `UnsatisfiedLinkError`. Mitigation: a preflight check in `scripts/check_android_aar.sh` that fails fast with `(AAR not built — run 'make android-aar' first)` so the developer sees the right next step.
- A CI run without the NDK should not fail the lint pipeline. Mitigation: the `make android-test` target prints the skip message and exits 0 when the preflight check returns non-zero.

**Success Signal:** `make android-test` on this host prints `==> syauth_mobile.aar not built (run 'make android-aar' on an NDK host); skipping` and exits 0.

### Friction and Opportunity

| Friction | Phase | Opportunity |
|----------|-------|-------------|
| Most dev hosts have no Android SDK / NDK / emulator | All | `make android-test` skips cleanly with actionable messages, never hangs. |
| Compose / AGP / Kotlin version drift breaks the build | Phase 1 | Pin every version to prrr-android's proven set; document each in this journey. |
| Hand-written JNI is easy to accidentally introduce | Phase 2 | Grep guard in `make android-test`; documented expectation in the journey + AGENTS.md. |
| APK bloat from copy-pasting prrr-android's full dep graph | Phase 4 | Drop CameraX / ZXing / security-crypto / coroutines from S-015's deps; document the omission. |

### North Star Summary

Sam runs `make android-aar && make android-test` on an NDK + SDK + emulator host once and sees `OOB: 🐱 🐶 🐭 🐰` rendered on the screen, proving the entire Rust→Kotlin pipeline works. Every future phone screen (pairing, approve, background bridge) is built on this same scaffold without touching Gradle wiring. On hosts without the toolchain, the target skips cleanly with a one-line message — no surprises, no broken CI.

## 3. UX Implementation and Assessment

### Time to First Value
- [x] `make android-aar && make android-test` is two commands from a clean clone to a green test.
- [x] The hello-world screen is a single file under 50 lines.

### Onboarding Clarity
- [x] `syauth-android/README.md` updates with the run command.
- [x] `make help` mentions `android-test`.

### Production-Ready Defaults
- [x] No emojis in Gradle / Kotlin source (the OOB words are *data*, not source decoration).
- [x] No `abiFilters` — the AAR ships every ABI Android Studio's bundled NDK supports.

### Golden Path Quality
- [x] Instrumented test asserts the rendered text starts with `"OOB: "` AND is at least 6 chars after the prefix (4 emoji words separated by spaces).
- [x] The test rule launches MainActivity, not a synthetic test activity — proves real-app behavior.

### Decision Load
- [x] One Compose screen. One bond_key fixture. Zero conditional UI states in S-015.
- [x] No theme customization beyond `MaterialTheme { ... }`.

### Progressive Complexity
- [x] Adding a second screen (S-016) is one new Composable + one navigation hop in MainActivity.
- [x] The Gradle plumbing supports it without changes.

### Error Quality
- [x] `make android-test` distinguishes "AAR missing", "no emulator", and "test failed" with separate messages.
- [x] The Compose `runCatching` wraps the UniFFI call so a binding regression renders `ERR: <message>` rather than crashing.

### Failure Safety
- [x] AndroidManifest has zero permissions (no `INTERNET`, no `BLUETOOTH_*`) — the hello-world cannot exfiltrate or pair.
- [x] No `applicationId` collision: `com.sy.syauth.android` is unique within the org.

### Runtime Transparency
- [x] The rendered string is the receipt for "Rust call succeeded". No hidden log-only state.

### Anti-Bypass Design
- [x] `make android-test` greps for `external fun` in `syauth-android/` — every Rust call goes through UniFFI.

## 4. Mirror of prrr-android

S-015 is a faithful mirror of `~/sources/prrr/prrr-android`. Each row below names the file in prrr-android, the corresponding file in syauth-android, and the only deltas.

| prrr-android | syauth-android | Delta |
|--------------|----------------|-------|
| `settings.gradle.kts` (`pluginManagement { repositories { google(); mavenCentral(); gradlePluginPortal() } }` + `dependencyResolutionManagement { repositoriesMode = FAIL_ON_PROJECT_REPOS; repositories { google(); mavenCentral() } }`, `include(":app")`) | `syauth-android/settings.gradle.kts` | `rootProject.name = "syauth-android"` (was `"prrr VPN"`). |
| `build.gradle.kts` (`plugins { id("com.android.application") version "8.2.2" apply false; id("org.jetbrains.kotlin.android") version "1.9.22" apply false }`) | `syauth-android/build.gradle.kts` | None. |
| `gradle.properties` (`kotlin.code.style=official`, `android.useAndroidX=true`, `android.defaults.buildfeatures.buildconfig=true`, `org.gradle.daemon=true`, `org.gradle.parallel=true`, `org.gradle.caching=true`, `org.gradle.jvmargs=-Xmx2048m -Dfile.encoding=UTF-8`) | `syauth-android/gradle.properties` | Drop the `org.gradle.java.home=/usr/lib/jvm/java-21-openjdk` line (host-specific in prrr; JVM 17+ is required, the developer's `JAVA_HOME` decides). |
| `gradle/wrapper/gradle-wrapper.properties` (`distributionUrl=https\://services.gradle.org/distributions/gradle-9.5.0-milestone-3-bin.zip`) | `syauth-android/gradle/wrapper/gradle-wrapper.properties` | None. |
| `gradle/wrapper/gradle-wrapper.jar` | `syauth-android/gradle/wrapper/gradle-wrapper.jar` | Copied verbatim. |
| `gradlew` (script, 249 lines) | `syauth-android/gradlew` | Copied verbatim. |
| `gradlew.bat` | `syauth-android/gradlew.bat` | Copied verbatim. |
| `app/build.gradle.kts` `android { namespace = "com.prrr.vpn.android"; compileSdk = 34; defaultConfig { applicationId = "com.prrr.vpn.android"; minSdk = 26; targetSdk = 34; versionCode = 1; versionName = "1.0.0"; testInstrumentationRunner = "androidx.test.runner.AndroidJUnitRunner" }; compileOptions { sourceCompatibility = VERSION_17; targetCompatibility = VERSION_17 }; kotlinOptions { jvmTarget = "17" }; buildFeatures { compose = true }; composeOptions { kotlinCompilerExtensionVersion = "1.5.8" }; packaging { resources { excludes += "/META-INF/{AL2.0,LGPL2.1}" } } }` | `syauth-android/app/build.gradle.kts` | namespace + applicationId → `com.sy.syauth.android`. Drop signingConfigs (S-015 ships debug-only — release signing lands later). Drop `release { isMinifyEnabled = true; isShrinkResources = true; proguardFiles(...) }` for the same reason. Drop the `testOptions { unitTests.isReturnDefaultValues = true }` block (no Robolectric unit tests in S-015). Drop the `vectorDrawables { useSupportLibrary = true }` (no vector drawables in hello-world). |
| `app/build.gradle.kts` `dependencies { implementation(files("libs/prrr_mobile.aar")); implementation("net.java.dev.jna:jna:5.14.0@aar"); implementation("androidx.core:core-ktx:1.12.0"); implementation("androidx.lifecycle:lifecycle-runtime-ktx:2.7.0"); implementation("androidx.activity:activity-compose:1.8.2"); implementation(platform("androidx.compose:compose-bom:2024.02.00")); implementation("androidx.compose.ui:ui"); implementation("androidx.compose.ui:ui-graphics"); implementation("androidx.compose.ui:ui-tooling-preview"); implementation("androidx.compose.material3:material3"); androidTestImplementation("androidx.test.ext:junit:1.1.5"); androidTestImplementation("androidx.test.espresso:espresso-core:3.5.1"); androidTestImplementation(platform("androidx.compose:compose-bom:2024.02.00")); androidTestImplementation("androidx.compose.ui:ui-test-junit4"); debugImplementation("androidx.compose.ui:ui-tooling"); debugImplementation("androidx.compose.ui:ui-test-manifest"); }` | `syauth-android/app/build.gradle.kts` `dependencies { ... }` | AAR path changes to `files("../../crates/syauth-mobile/target/syauth_mobile.aar")` (S-014's build output, no `app/libs/` shim). Drop `lifecycle-viewmodel-compose`, `kotlinx-coroutines-android`, `androidx.security:security-crypto`, CameraX (4 entries), `com.google.zxing:core` — none are needed for the hello-world. Drop the `test*` dependencies (junit, robolectric, androidx.test:core, kotlinx-coroutines-test) — no JVM unit tests in S-015; the connectedAndroidTest path is sufficient. |
| `app/src/main/AndroidManifest.xml` (4 permissions: INTERNET, CAMERA, FOREGROUND_SERVICE, FOREGROUND_SERVICE_SPECIAL_USE; `<application android:theme="@style/Theme.PrrrVPN" .../>`; MainActivity + intent-filter for `android.intent.action.MAIN` + deep links; VPN service) | `syauth-android/app/src/main/AndroidManifest.xml` | **Zero permissions.** No FOREGROUND_SERVICE, no INTERNET. No Bluetooth permissions (BT lands in S-018). No deep-link intent-filter (no `syauth://` scheme yet — pairing lands in S-016). No service declarations. The omission is documented in this journey. |
| `app/src/main/res/values/themes.xml` (`Theme.PrrrVPN` parent Material.Light.NoActionBar) | `syauth-android/app/src/main/res/values/themes.xml` | `Theme.SyauthAndroid` parent same. |
| `app/src/main/res/values/strings.xml` (`<string name="app_name">Prrr</string>`) | `syauth-android/app/src/main/res/values/strings.xml` | `<string name="app_name">syauth</string>`. |
| `app/src/main/kotlin/com/prrr/vpn/android/MainActivity.kt` (200-line ComponentActivity with deep-link handling, multi-screen navigation, ViewModelFactory) | `syauth-android/app/src/main/kotlin/com/sy/syauth/android/MainActivity.kt` | Trimmed to a single `ComponentActivity` whose `setContent { MaterialTheme { OobScreen() } }` calls `uniffi.syauth_mobile.oobCodeForBond(...)` once on first composition and renders `Text("OOB: $words")`. |
| `app/src/androidTest/kotlin/com/prrr/vpn/android/UniffiParseQrConfigTest.kt` (instrumented test using `@RunWith(AndroidJUnit4::class)` calling `uniffi.prrr_mobile.parseQrConfig(...)`) | `syauth-android/app/src/androidTest/kotlin/com/sy/syauth/android/HelloWorldTest.kt` | Switches to `createAndroidComposeRule<MainActivity>()` to assert the rendered text. Calls `uniffi.syauth_mobile.oobCodeForBond(...)` directly as the rendered-output reference. |

### Permissions omitted (audit trail)

The hello-world manifest declares **zero permissions** because S-015 has no I/O. The intentionally-omitted permissions, each named in this journey so a reader doesn't think the manifest is incomplete:

- `android.permission.INTERNET` — no network calls in S-015. The protocol talks BLE, not TCP/UDP.
- `android.permission.BLUETOOTH_CONNECT`, `_SCAN`, `_ADVERTISE` — Bluetooth lands in **S-018** (`CompanionDeviceService`). Adding them now would lie about the app's surface to the user and the manifest reviewer.
- `android.permission.POST_NOTIFICATIONS` — notifications land in S-018 alongside the background bridge.
- `android.permission.USE_BIOMETRIC` — BiometricPrompt lands in S-017.

### Dependencies omitted (audit trail)

The dep graph drops every prrr-android dep that S-015 doesn't need:

- **CameraX (4 modules)** — needed for QR scanning in prrr-android. Not in syauth's protocol (invite is delivered via syauth:// URI from CLI, scanned later by S-016 if/when QR pairing lands).
- **`com.google.zxing:core`** — same.
- **`androidx.security:security-crypto`** — bond key storage lands in S-016+ alongside the pairing screen, where the Keystore-backed storage will be wired with the right `setUserAuthenticationRequired` policy.
- **`androidx.lifecycle:lifecycle-viewmodel-compose`** — S-015 has no ViewModel; the OOB call is a one-shot in `LaunchedEffect`.
- **`kotlinx-coroutines-android`** — same; no coroutines in S-015.
- **`junit:junit`, `org.robolectric:robolectric`, `androidx.test:core`, `kotlinx-coroutines-test`** — Robolectric JVM tests land alongside the pairing state machine in S-016.

### Hand-written JNI rule (DoD #4)

The DoD line "No hand-written JNI in the codebase. Every Rust call goes through the UniFFI-generated Kotlin." is enforced two ways:

1. **Mechanical** — `make android-test` runs `grep -rn 'external fun' syauth-android/` before delegating to Gradle; non-zero match count fails the target with a named error.
2. **Documentary** — the only Kotlin file under `syauth-android/app/src/main/kotlin/uniffi/syauth_mobile/` is the file produced by `uniffi-bindgen generate ... --language kotlin`, copied in by `make android-aar`. The bindings file carries the canonical "This file was autogenerated by some hot garbage in the `uniffi` crate" banner; reviewers see at a glance it's not hand-written. No hand-written JNI exists, and no source file under `com/sy/syauth/android/` issues a `System.loadLibrary(...)` call — JNA loads the `.so` from the AAR's `jni/<abi>/` automatically.

### AAR consumption path (host-portable)

The DoD asks the Gradle project to consume `crates/syauth-mobile/target/syauth_mobile.aar`. Two operational facts shape the wiring:

- S-014's `make android-aar` requires an Android NDK. Most developer hosts (and this CI host) don't have one — `make android-aar-dry-run` is the local sanity check.
- The Kotlin bindings file (`syauth_mobile.kt`) is generated alongside the AAR by `uniffi-bindgen`. The build script copies it to `crates/syauth-mobile/bindings/kotlin/uniffi/syauth_mobile/syauth_mobile.kt`.

We thread the needle by:

- Pointing the Gradle dep at the AAR via a path that is stable across hosts: `implementation(files("../crates/syauth-mobile/target/syauth_mobile.aar"))`.
- Copying the generated Kotlin into `syauth-android/app/src/main/kotlin/uniffi/syauth_mobile/syauth_mobile.kt` *only when the AAR build runs* (a follow-up `make android-aar` step appended to `scripts/build_aar.sh`). For S-015's scaffold to compile on a host without the AAR, the source tree carries a hand-pruned UniFFI stub committed as `syauth_mobile.kt` — but that approach would violate DoD #4 ("No hand-written JNI"). Instead, we ship a **`scripts/check_android_aar.sh`** preflight that fails fast if the AAR is missing, and `make android-test` skips with a clear message in that case. On an NDK host, the AAR builds first; the binding file is dropped into the Kotlin source set by the build script (extends `scripts/build_aar.sh`).
- The "skip if no AAR" path is the documented CI behavior for hosts without the NDK; the full pipeline (NDK + SDK + emulator) is what produces the green CI check.

### make android-test contract

The target sequences three checks before delegating to Gradle, each with a clear skip path:

1. **AAR present?** `scripts/check_android_aar.sh` — exits 0 if `crates/syauth-mobile/target/syauth_mobile.aar` exists; non-zero with an actionable message otherwise. The Make target catches the non-zero and exits 0 with `(syauth_mobile.aar not built — run 'make android-aar' on an NDK host; skipping)`.
2. **Emulator connected?** `adb devices` lists at least one `device`-state line. If not, print `(no Android emulator connected — skipping)` and exit 0.
3. **No hand-written JNI?** `grep -rn 'external fun' syauth-android/` returns zero hits. If non-zero, exit 1 with the matches printed.

If all three pass, run `cd syauth-android && ./gradlew :app:connectedAndroidTest` and propagate the exit code.

## 5. Tests

### TC-01: HelloWorldTest — instrumented Compose test asserts the OOB string is rendered

**Given** the emulator has the syauth-android debug APK installed and JNA can load `libsyauth_mobile.so` for the emulator's ABI.
**When** the test rule launches `MainActivity` via `createAndroidComposeRule<MainActivity>()`.
**Then** `onNodeWithText("OOB: ", substring = true).assertIsDisplayed()` passes AND the rendered text matches the regex `^OOB: \S+ \S+ \S+ \S+$` (four whitespace-separated words after the prefix).

### TC-02: `make android-test` skips cleanly with no AAR

**Given** `crates/syauth-mobile/target/syauth_mobile.aar` does not exist (no NDK on host).
**When** `make android-test` runs.
**Then** the target prints `(syauth_mobile.aar not built — run 'make android-aar' on an NDK host; skipping)` and exits 0.

### TC-03: `make android-test` skips cleanly with no emulator

**Given** the AAR exists but `adb devices` lists no `device`-state line.
**When** `make android-test` runs.
**Then** the target prints `(no Android emulator connected — skipping)` and exits 0.

### TC-04: No hand-written JNI

**Given** the syauth-android source tree.
**When** `grep -rn 'external fun' syauth-android/` runs.
**Then** zero matches are returned.

### TC-05: APK size budget

**Given** an SDK-equipped host with the AAR built.
**When** `./gradlew :app:assembleDebug` runs.
**Then** `app/build/outputs/apk/debug/app-debug.apk` exists and `du -k` reports a size below 10240 (10 MB).

### TC-06: Gradle wrapper matches prrr-android

**Given** `syauth-android/gradle/wrapper/gradle-wrapper.properties`.
**When** the file is read.
**Then** the `distributionUrl` equals prrr-android's `distributionUrl` (Gradle 9.5.0-milestone-3) byte-for-byte.

### TC-07: AGP / Kotlin / Compose versions match prrr-android

**Given** `syauth-android/build.gradle.kts` and `syauth-android/app/build.gradle.kts`.
**When** the version literals are read.
**Then** AGP `8.2.2`, Kotlin `1.9.22`, JNA `5.14.0`, Compose BOM `2024.02.00`, `kotlinCompilerExtensionVersion = "1.5.8"`, JVM target `17`, `compileSdk = 34`, `minSdk = 26`, `targetSdk = 34` — every value matches prrr-android.

## Traceability
- Roadmap item: [specs/syauth/ROADMAP.md §S-015](../syauth/ROADMAP.md).
- Implementation files:
  - `syauth-android/settings.gradle.kts`
  - `syauth-android/build.gradle.kts`
  - `syauth-android/gradle.properties`
  - `syauth-android/gradle/wrapper/gradle-wrapper.properties`
  - `syauth-android/gradle/wrapper/gradle-wrapper.jar`
  - `syauth-android/gradlew`, `syauth-android/gradlew.bat`
  - `syauth-android/app/build.gradle.kts`
  - `syauth-android/app/src/main/AndroidManifest.xml`
  - `syauth-android/app/src/main/res/values/{themes,strings}.xml`
  - `syauth-android/app/src/main/kotlin/com/sy/syauth/android/MainActivity.kt`
  - `scripts/check_android_aar.sh`
  - root `Makefile` (`android-test` target)
- Test files:
  - `syauth-android/app/src/androidTest/kotlin/com/sy/syauth/android/HelloWorldTest.kt`
