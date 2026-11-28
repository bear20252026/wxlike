import java.util.Properties

plugins {
    id("com.android.application")
    // The Flutter Gradle Plugin must be applied after the Android and Kotlin Gradle plugins.
    id("dev.flutter.flutter-gradle-plugin")
}

// CI（与持有 key.properties 的发布机）用这里的签名配置。
// **release 没有完整签名材料就必须失败**：不再回退 debug 签名，否则会产出
// debug-cert APK，破坏发布身份与升级路径（H-26）。
val keystoreProperties = Properties().apply {
    val f = rootProject.file("key.properties")
    if (f.exists()) {
        f.inputStream().use(::load)
    }
}
val signingPropertyNames = listOf("storeFile", "storePassword", "keyAlias", "keyPassword")
val missingSigningProperties = signingPropertyNames.filter {
    keystoreProperties.getProperty(it).isNullOrBlank()
}
val releaseKeystoreFile = keystoreProperties.getProperty("storeFile")
    ?.takeIf { it.isNotBlank() }
    ?.let { rootProject.file(it) }
val hasReleaseKeystore = missingSigningProperties.isEmpty() && releaseKeystoreFile?.isFile == true

android {
    namespace = "com.wxlike.wxlike_mobile"
    compileSdk = flutter.compileSdkVersion
    ndkVersion = flutter.ndkVersion

    if (hasReleaseKeystore) {
        signingConfigs {
            create("release") {
                keyAlias = keystoreProperties.getProperty("keyAlias")
                keyPassword = keystoreProperties.getProperty("keyPassword")
                storeFile = file(keystoreProperties.getProperty("storeFile"))
                storePassword = keystoreProperties.getProperty("storePassword")
            }
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    defaultConfig {
        // TODO: Specify your own unique Application ID (https://developer.android.com/studio/build/application-id.html).
        applicationId = "com.wxlike.wxlike_mobile"
        // You can update the following values to match your application needs.
        // For more information, see: https://flutter.dev/to/review-gradle-config.
        minSdk = flutter.minSdkVersion
        targetSdk = flutter.targetSdkVersion
        // Uses the version code from pubspec.yaml. When using split APKs, 1000 * ABI_VERSION
        // is added automatically by Flutter. (https://developer.android.com/studio/build/configure-apk-splits#configure-APK-versions)
        // You can force using the value of versionCode by specifying the `-P force-version-code-ignoring-abi=true`
        // flag during build.
        versionCode = flutter.versionCode
        versionName = flutter.versionName
    }

    buildTypes {
        release {
            // CI 与发布机都通过 key.properties 提供完整签名材料（见
            // ci/jobs/mobile/job.yml 与 release.yml）。缺任何一项都不配置签名，
            // 并由下面的 taskGraph 检查硬失败，绝不静默产出 debug-cert APK。
            if (hasReleaseKeystore) {
                signingConfig = signingConfigs.getByName("release")
            }
        }
    }
}

// release 任务一旦出现在 task graph，签名材料必须完整；否则在建任何产物前失败。
gradle.taskGraph.whenReady {
    val releaseRequested = allTasks.any { it.name.contains("Release") }
    if (releaseRequested && !hasReleaseKeystore) {
        val missing = if (missingSigningProperties.isEmpty()) {
            "storeFile does not exist: ${releaseKeystoreFile ?: "<unset>"}"
        } else {
            "missing properties: ${missingSigningProperties.joinToString(", ")}"
        }
        throw GradleException(
            "release build requires a complete apps/mobile/android/key.properties " +
                "($missing); debug signing is not a release fallback (H-26)"
        )
    }
}

kotlin {
    compilerOptions {
        jvmTarget = org.jetbrains.kotlin.gradle.dsl.JvmTarget.JVM_17
    }
}

flutter {
    source = "../.."
}
