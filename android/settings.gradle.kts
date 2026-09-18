// 只有一个模块：native 代码由 `cargo ndk` 编成 `src/main/jniLibs/<abi>/libToDay.so`（见 scripts/build-android.ps1）
pluginManagement {
    repositories {
        google()
        mavenCentral()
        gradlePluginPortal()
    }
}

dependencyResolutionManagement {
    repositories {
        google()
        mavenCentral()
    }
}

rootProject.name = "ToDay"
include(":app")
