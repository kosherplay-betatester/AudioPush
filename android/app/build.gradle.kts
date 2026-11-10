plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
}

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
        versionCode = 1
        versionName = "0.1.0"
    }

    buildTypes {
        release {
            isMinifyEnabled = false
            // Debug-signed so `assembleRelease` produces an installable APK
            // without a keystore. Replace with your own signing config before
            // publishing anywhere that matters.
            signingConfig = signingConfigs.getByName("debug")
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
