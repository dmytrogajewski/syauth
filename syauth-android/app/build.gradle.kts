// Roadmap item S-015 — mirrors `~/sources/prrr/prrr-android/app/build.gradle.kts`.
//
// Every version pinned here matches prrr-android's proven set:
//   AGP 8.2.2, Kotlin 1.9.22 (declared in root build.gradle.kts)
//   compileSdk = 34, minSdk = 26, targetSdk = 34
//   JVM 17, Compose compiler extension 1.5.8
//   JNA 5.14.0, Compose BOM 2024.02.00
//
// Deviations from prrr-android (audited in
// specs/journeys/JOURNEY-S-015-android-scaffold.md):
//   - namespace + applicationId -> com.sy.syauth.android
//   - AAR path -> files("../../crates/syauth-mobile/target/syauth_mobile.aar")
//   - signingConfigs and release-build minify dropped (S-015 is debug-only)
//   - vectorDrawables block dropped (no drawables yet)
//   - CameraX / ZXing / security-crypto dropped (none used by syauth;
//     CameraX/ZXing are prrr-specific QR scanner deps)
//
// Deltas since S-015 (additive — every S-015 contract still holds):
//   S-016 (pairing screen):
//     - testOptions { unitTests.isIncludeAndroidResources = true;
//                     unitTests.isReturnDefaultValues = true } added for
//       the Robolectric tests in pair/PairingViewModelTest.kt
//     - androidx.lifecycle:lifecycle-viewmodel-compose:2.7.0 added for
//       the viewModel() composable in MainActivity.kt
//     - androidx.navigation:navigation-compose:2.7.7 added for the
//       multi-route NavHost wiring in MainActivity.kt
//     - junit / Robolectric / androidx.test JVM-side test deps added
//   S-017 (approve screen + BiometricPrompt + Keystore signer):
//     - androidx.biometric:biometric:1.2.0-alpha05 (BiometricPrompt +
//       CryptoObject)
//     - androidx.lifecycle:lifecycle-viewmodel-ktx:2.7.0 (viewModelScope
//       + StateFlow plumbing)
//     - androidx.lifecycle:lifecycle-runtime-compose:2.7.0
//       (collectAsStateWithLifecycle)
//     - androidx.fragment:fragment-ktx:1.6.2 (FragmentActivity host that
//       BiometricPrompt binds to)
//     - androidx.compose.material:material-icons-core (Lock app icon
//       on the Approve screen)
//     - androidx.arch.core:core-testing:2.2.0 +
//       org.jetbrains.kotlinx:kotlinx-coroutines-test:1.7.3 on
//       testImplementation for ApproveViewModelTest.kt

plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
}

android {
    namespace = "com.sy.syauth.android"
    compileSdk = 34

    defaultConfig {
        applicationId = "com.sy.syauth.android"
        minSdk = 26
        targetSdk = 34
        versionCode = 1
        versionName = "0.1.0"

        testInstrumentationRunner = "androidx.test.runner.AndroidJUnitRunner"
    }

    // S-016: Robolectric loads the framework resources from the merged
    // resources tree at test time. Without `isIncludeAndroidResources =
    // true`, `@Config(sdk = [34])` fails with `Resources$NotFoundException`
    // on the first `MaterialTheme` lookup. Mirrors prrr-android's setting.
    // S-017's ApproveViewModelTest is pure JVM, but the option is still
    // needed for the pair tests and is harmless either way.
    testOptions {
        unitTests.isIncludeAndroidResources = true
        unitTests.isReturnDefaultValues = true
    }

    buildTypes {
        release {
            // S-015 is the debug-only hello-world; release signing + R8 land
            // once we have a screen worth shipping. Per AGENTS.md, we leave
            // the block declared but minimal so a future agent doesn't have
            // to invent the structure from scratch.
            isMinifyEnabled = false
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    kotlinOptions {
        jvmTarget = "17"
    }

    buildFeatures {
        compose = true
    }

    composeOptions {
        kotlinCompilerExtensionVersion = "1.5.8"
    }

    packaging {
        resources {
            excludes += "/META-INF/{AL2.0,LGPL2.1}"
        }
    }

    // The Kotlin source set adds the UniFFI-generated bindings directory so
    // `uniffi.syauth_mobile.*` is in the classpath. The bindings file is
    // produced by `scripts/build_aar.sh` and lives outside the Android
    // source tree to keep generated artifacts off the commit graph.
    sourceSets {
        getByName("main") {
            kotlin.srcDirs(
                "src/main/kotlin",
                "../../crates/syauth-mobile/bindings/kotlin"
            )
        }
    }
}

dependencies {
    // Rust FFI surface produced by `make android-aar` (S-014). The AAR
    // packages `libsyauth_mobile.so` per ABI under `jni/<abi>/`; JNA loads
    // the right one at runtime, no `System.loadLibrary` call needed.
    implementation(files("../../crates/syauth-mobile/target/syauth_mobile.aar"))
    implementation("net.java.dev.jna:jna:5.14.0@aar")

    // AndroidX Core — the bare minimum to run a ComponentActivity with Compose.
    implementation("androidx.core:core-ktx:1.12.0")
    implementation("androidx.lifecycle:lifecycle-runtime-ktx:2.7.0")
    // S-016: ViewModel base class for PairingViewModel + viewModel() composable.
    // S-017: same dep also covers ApproveViewModel.
    implementation("androidx.lifecycle:lifecycle-viewmodel-compose:2.7.0")
    // S-017: viewModelScope + StateFlow plumbing.
    implementation("androidx.lifecycle:lifecycle-viewmodel-ktx:2.7.0")
    // S-017: collectAsStateWithLifecycle for ApproveScreen.
    implementation("androidx.lifecycle:lifecycle-runtime-compose:2.7.0")
    // S-017: FragmentActivity host required by BiometricPrompt (it binds
    // to the fragment manager for its dialog lifecycle).
    implementation("androidx.fragment:fragment-ktx:1.6.2")
    implementation("androidx.activity:activity-compose:1.8.2")

    // S-016: Navigation between the home, pair, and (S-017) approve routes.
    // Adding navigation-compose is cheaper (one dep, ~50 KB) than
    // hand-rolling a mutableStateOf-based router and reads like a real
    // app structure to the next contributor.
    implementation("androidx.navigation:navigation-compose:2.7.7")

    // S-017: BiometricPrompt + CryptoObject for the Approve gate.
    implementation("androidx.biometric:biometric:1.2.0-alpha05")

    // Jetpack Compose — Material3 surface + Text.
    implementation(platform("androidx.compose:compose-bom:2024.02.00"))
    implementation("androidx.compose.ui:ui")
    implementation("androidx.compose.ui:ui-graphics")
    implementation("androidx.compose.ui:ui-tooling-preview")
    implementation("androidx.compose.material3:material3")
    // S-017: Material `Lock` icon for the Approve screen header.
    implementation("androidx.compose.material:material-icons-core")

    // JVM-side unit tests:
    //   S-016: Robolectric @Config(sdk = [34]) for PairingViewModelTest
    //          (subclasses androidx.lifecycle.ViewModel which touches
    //          framework code). Hand-rolled fakes, no mockk.
    //   S-017: ApproveViewModelTest is pure JVM (every Android side-effect
    //          is injected behind an interface), but the Robolectric
    //          runtime stays on the classpath so future tests can opt in.
    testImplementation("junit:junit:4.13.2")
    testImplementation("org.robolectric:robolectric:4.11.1")
    testImplementation("androidx.test:core:1.5.0")
    testImplementation("androidx.test.ext:junit:1.1.5")
    // S-017: InstantTaskExecutorRule + LiveData/StateFlow plumbing for
    //        ApproveViewModelTest.
    testImplementation("androidx.arch.core:core-testing:2.2.0")
    // S-017: TestDispatcher + advanceTimeBy for the countdown test.
    testImplementation("org.jetbrains.kotlinx:kotlinx-coroutines-test:1.7.3")

    // Instrumented test surface — Compose UI test rule + JUnit4 wiring.
    androidTestImplementation("androidx.test.ext:junit:1.1.5")
    androidTestImplementation("androidx.test.espresso:espresso-core:3.5.1")
    androidTestImplementation(platform("androidx.compose:compose-bom:2024.02.00"))
    androidTestImplementation("androidx.compose.ui:ui-test-junit4")

    // Compose tooling + the manifest that registers the test ComponentActivity
    // host. Without `ui-test-manifest`, `createAndroidComposeRule<MainActivity>`
    // fails with ActivityNotFoundException at test time.
    debugImplementation("androidx.compose.ui:ui-tooling")
    debugImplementation("androidx.compose.ui:ui-test-manifest")
}
