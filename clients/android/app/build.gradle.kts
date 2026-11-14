plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
    id("org.jetbrains.kotlin.plugin.compose")
}

// One version for everything: `[workspace.package] version` in crates/Cargo.toml
// (see AGENTS.md). versionName reads it; nothing here restates it.
val workspaceVersion: String = rootDir.resolve("../../crates/Cargo.toml").readLines()
    .dropWhile { it.trim() != "[workspace.package]" }
    .drop(1)
    .firstNotNullOfOrNull { Regex("""^version\s*=\s*"([^"]+)"""").find(it.trim())?.groupValues?.get(1) }
    ?: error("could not read [workspace.package] version from crates/Cargo.toml")

android {
    namespace = "com.unpeel.android"
    compileSdk = 35

    defaultConfig {
        // Same package as the published phone app, so this APK replaces android-0.7.7-33.
        applicationId = "com.unpeel.android"
        minSdk = 26
        targetSdk = 35
        // Build number, a separate monotonic space from the version (like the
        // Mac app's CFBundleVersion). Bump it for every APK that is published.
        versionCode = 45
        versionName = workspaceVersion
    }

    // Optional release keystore via env (CI secrets). Without it, release is
    // still signed with the debug key so the APK sideloads; never published as
    // a GitHub Release from this branch.
    val releaseStoreFile = System.getenv("ANDROID_KEYSTORE_FILE")
    if (releaseStoreFile != null) {
        signingConfigs {
            create("release") {
                storeFile = file(releaseStoreFile)
                storePassword = System.getenv("ANDROID_KEYSTORE_PASSWORD")
                keyAlias = System.getenv("ANDROID_KEY_ALIAS")
                keyPassword = System.getenv("ANDROID_KEY_PASSWORD")
            }
        }
    }

    buildTypes {
        release {
            isMinifyEnabled = false
            isDebuggable = false
            signingConfig = if (releaseStoreFile != null) {
                signingConfigs.getByName("release")
            } else {
                signingConfigs.getByName("debug")
            }
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
        buildConfig = true
    }
}

dependencies {
    implementation(project(":protocol"))
    implementation(project(":termit"))
    implementation(platform("androidx.compose:compose-bom:2024.10.01"))
    implementation("androidx.compose.ui:ui")
    implementation("androidx.compose.material3:material3")
    implementation("androidx.compose.foundation:foundation")
    implementation("androidx.activity:activity-compose:1.9.3")
    implementation("androidx.core:core-ktx:1.15.0")
    implementation("org.jetbrains.kotlinx:kotlinx-coroutines-android:1.9.0")
    implementation("com.squareup.okhttp3:okhttp:4.12.0")
    implementation("com.journeyapps:zxing-android-embedded:4.3.0")
    testImplementation("junit:junit:4.13.2")
}
