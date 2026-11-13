plugins {
    id("com.android.application")
}

android {
    namespace = "com.pemozone.client"
    compileSdk = 35

    defaultConfig {
        applicationId = "com.pemozone.client"
        // Android 10, the first with a Vulkan 1.1 loader everywhere.
        minSdk = 29
        targetSdk = 35
        versionCode = 1
        versionName = "0.1"
        ndk {
            abiFilters += "arm64-v8a"
        }
    }
}
