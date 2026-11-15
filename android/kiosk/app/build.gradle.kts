plugins {
    id("com.android.application")
}

android {
    namespace = "io.github.ryanhecht.layerview.kiosk"
    compileSdk = 36

    defaultConfig {
        applicationId = "io.github.ryanhecht.layerview.kiosk"
        minSdk = 29
        targetSdk = 36
        versionCode = 1
        versionName = "0.1.0"
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
}
