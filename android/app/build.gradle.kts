// 一个纯 native 的 APK：没有 Java / Kotlin 源码。
//
// 界面与逻辑全在 `libToDay.so` 里（`android_main`），Java 那侧只需要
// `androidx.games` 的 `GameActivity` 作为壳把 `Surface`/输入/IME 转给 native 代码。
plugins {
    id("com.android.application")
}

android {
    // 包名与数据目录绑死：`/data/user/0/com.today.diary/files/today.db`。
    // 装上去之后再改，用户就看不到自己的日记了。
    namespace = "com.today.diary"
    compileSdk = 35

    defaultConfig {
        applicationId = "com.today.diary"
        // 26：自适应图标与 `DroidSansFallback` 都在这个版本之后，且不需要为老系统做兼容分支
        minSdk = 26
        targetSdk = 35
        versionCode = 1
        versionName = "0.1.0"
    }

    buildTypes {
        release {
            // 混淆与资源压缩对"只有一个 .so"的 APK 没有意义，开了只会多一层出错的可能
            isMinifyEnabled = false
            isShrinkResources = false
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
}

// Kotlin 1.8 起 `kotlin-stdlib-jdk7/jdk8` 就是空壳（内容并进了 `kotlin-stdlib`），
// 而 appcompat 那条链（lifecycle → kotlinx-coroutines → ）与 games-activity 都还引着 1.6.21 的
// 两个独立构件：它们与 `kotlin-stdlib` 1.8.22 同时在场就是“重复类”，
// `checkDebugDuplicateClasses` 会直接卡停构建。两个构件在这里一次排干净；
// 真正的字节码在 `kotlin-stdlib` 里一个不少。
configurations.configureEach {
    exclude(group = "org.jetbrains.kotlin", module = "kotlin-stdlib-jdk7")
    exclude(group = "org.jetbrains.kotlin", module = "kotlin-stdlib-jdk8")
}

dependencies {
    // GameActivity 的 Java 壳（manifest 里那个 Activity 类）；C++ glue 由 android-activity 自己带，
    // 所以**不要**打开 prefab（见 android-activity 的 README）
    implementation("androidx.games:games-activity:4.4.0")
    // GameActivity 继承 AppCompatActivity，主题必须是 AppCompat 的后代，显式写出来免得靠传递依赖
    implementation("androidx.appcompat:appcompat:1.7.0")
}
