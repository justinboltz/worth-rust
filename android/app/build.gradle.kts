import java.net.URI
import org.jetbrains.kotlin.gradle.dsl.JvmTarget

plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
}

val worthBaseUrl = providers.gradleProperty("worthBaseUrl").orElse("http://localhost:3001").get().trimEnd('/')
val endpoint = URI(worthBaseUrl)
require(endpoint.scheme in listOf("http", "https") && !endpoint.host.isNullOrBlank() &&
    endpoint.rawUserInfo == null && endpoint.rawQuery == null && endpoint.rawFragment == null &&
    endpoint.path.isNullOrEmpty()) { "worthBaseUrl must be an HTTP(S) origin without a path or credentials." }
require(endpoint.scheme == "https" || endpoint.host in listOf("localhost", "127.0.0.1", "10.0.2.2")) {
    "HTTP is supported only for local development. Use HTTPS for a hosted server."
}

android {
    namespace = "app.worth.mobile"
    compileSdk = 36
    buildToolsVersion = "36.0.0"

    defaultConfig {
        applicationId = "app.worth.mobile"
        minSdk = 28
        targetSdk = 36
        versionCode = 1
        versionName = "0.1.0"
        buildConfigField("String", "BASE_URL", "\"$worthBaseUrl\"")
        testInstrumentationRunner = "androidx.test.runner.AndroidJUnitRunner"
    }

    signingConfigs {
        getByName("debug") {
            storeFile = rootProject.file(".local/debug.keystore")
        }
    }

    buildTypes {
        debug {
            applicationIdSuffix = ".rustdebug"
            versionNameSuffix = "-dev"
            resValue("string", "app_name", "Worth Dev")
        }
        release {
            resValue("string", "app_name", "Worth")
            isMinifyEnabled = false
        }
    }

    buildFeatures { buildConfig = true }
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
}

// A production build requires an explicitly configured HTTPS backend.
androidComponents {
    beforeVariants(selector().withBuildType("release")) {
        it.enable = endpoint.scheme == "https"
    }
}

kotlin { compilerOptions { jvmTarget.set(JvmTarget.JVM_17) } }

dependencies {
    implementation("dev.hotwire:core:1.4.1")
    implementation("dev.hotwire:navigation-fragments:1.4.1")
    implementation("com.google.android.material:material:1.12.0")
    implementation("androidx.browser:browser:1.10.0")
    testImplementation("junit:junit:4.13.2")
}
