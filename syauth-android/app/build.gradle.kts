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
//   - testOptions, vectorDrawables blocks dropped (no Robolectric / drawables yet)
//   - CameraX / ZXing / security-crypto / coroutines / lifecycle-viewmodel-compose
//     dropped (none used by the hello-world; land in S-016+)
//   - JVM-side unit-test deps dropped (no Robolectric in S-015)

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
    implementation("androidx.activity:activity-compose:1.8.2")

    // Jetpack Compose — single screen, Material3 surface + Text.
    implementation(platform("androidx.compose:compose-bom:2024.02.00"))
    implementation("androidx.compose.ui:ui")
    implementation("androidx.compose.ui:ui-graphics")
    implementation("androidx.compose.ui:ui-tooling-preview")
    implementation("androidx.compose.material3:material3")

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
