import java.util.Properties

plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
}

// Release signing details, kept out of version control.
//
// `signing/keystore.properties` and the keystore it points at are gitignored:
// they are this machine's key, not part of the source. Without them the build
// still works and falls back to the debug key, so anyone can clone and build,
// they just cannot produce an update that installs over an official release.
val keystoreProperties = Properties().apply {
    val file = rootProject.file("signing/keystore.properties")
    if (file.exists()) {
        file.inputStream().use { load(it) }
    }
}
val hasReleaseKey = keystoreProperties.getProperty("storeFile") != null

android {
    namespace = "com.kosherplay.audiopush"
    compileSdk = 35

    defaultConfig {
        applicationId = "com.kosherplay.audiopush"
        // Android 8.0: PERFORMANCE_MODE_LOW_LATENCY and adaptive icons.
        // Sending (AudioPlaybackCapture) additionally needs Android 10, which
        // is checked at runtime.
        minSdk = 26
        targetSdk = 35
        versionCode = 4
        versionName = "0.1.3"
    }

    signingConfigs {
        if (hasReleaseKey) {
            create("release") {
                storeFile = rootProject.file(keystoreProperties.getProperty("storeFile"))
                storePassword = keystoreProperties.getProperty("storePassword")
                keyAlias = keystoreProperties.getProperty("keyAlias")
                keyPassword = keystoreProperties.getProperty("keyPassword")
                // Both signing schemes: v2 is what modern Android verifies,
                // v1 keeps the APK installable on older releases.
                enableV1Signing = true
                enableV2Signing = true
            }
        }
    }

    buildTypes {
        release {
            // Shrink and obfuscate. There is no reflection anywhere in this
            // app, so the default rules are enough.
            isMinifyEnabled = true
            isShrinkResources = true
            isDebuggable = false
            proguardFiles(
                getDefaultProguardFile("proguard-android-optimize.txt"),
                "proguard-rules.pro"
            )
            signingConfig = if (hasReleaseKey) {
                signingConfigs.getByName("release")
            } else {
                // Lets a fresh clone still produce an installable APK.
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
}

dependencies {
    implementation("androidx.core:core-ktx:1.15.0")
    implementation("androidx.appcompat:appcompat:1.7.0")
    implementation("androidx.activity:activity-ktx:1.9.3")
    implementation("androidx.annotation:annotation:1.9.1")
}
