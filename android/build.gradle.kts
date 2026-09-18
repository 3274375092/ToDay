// 根 build 文件：只声明插件版本，具体配置在 app/build.gradle.kts
//
// 版本组合是本机验证过的：JDK 17 + Gradle 8.9 + AGP 8.7.3（compileSdk 35）。
// 升级 AGP 时要一起看 Gradle 与 JDK 的最低版本要求。
plugins {
    id("com.android.application") version "8.7.3" apply false
}
