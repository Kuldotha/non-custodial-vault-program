// The vault's client for Kotlin: its program id, addresses, instruction builders and account
// decoders, for Android and the browser. Lives beside the program so the two change together.
// Plugin versions come from the consuming build's root project, which also includes
// :solana-client.
plugins {
    id("com.android.library")
    id("org.jetbrains.kotlin.multiplatform")
}

@OptIn(org.jetbrains.kotlin.gradle.ExperimentalWasmDsl::class)
kotlin {
    wasmJs { browser() }
    androidTarget {
        compilerOptions {
            jvmTarget.set(org.jetbrains.kotlin.gradle.dsl.JvmTarget.JVM_17)
        }
    }

    sourceSets {
        commonMain.dependencies {
            api(project(":solana-client"))
        }
    }
}

android {
    namespace = "com.interstellargames.vault"
    compileSdk = 36
    defaultConfig { minSdk = 26 }
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
}
