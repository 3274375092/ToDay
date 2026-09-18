<#
.SYNOPSIS
  一条命令出 APK：先 cargo ndk 把 libToDay.so 编进 jniLibs，再交给 Gradle 打包。

.DESCRIPTION
  只用 cargo-ndk + Gradle，不引 cargo-apk / cargo-mobile2 / Mozilla 的 Gradle 插件。
  需要的东西（缺哪个脚本会直接说）：
    cargo-ndk          cargo install cargo-ndk
    Android SDK        ANDROID_HOME / ANDROID_SDK_ROOT，或 android/local.properties 里的 sdk.dir
    Android NDK        ANDROID_NDK_HOME / ANDROID_NDK_ROOT / NDK_HOME 之一，或 SDK 下的 ndk/
    JDK 17             JAVA_HOME
  产物：android/app/build/outputs/apk/<debug|release>/app-<debug|release>.apk

.EXAMPLE
  pwsh scripts/build-android.ps1                  # 真机用的 arm64-v8a + debug APK
  pwsh scripts/build-android.ps1 -Abi x86_64      # 模拟器
  pwsh scripts/build-android.ps1 -Release         # release 的 .so 与 APK
#>
# 注意：本文件存为 UTF-8 **带 BOM**。Windows PowerShell 5.1 不带 BOM 时会按 GBK 读，
# 中文注释的字节会吃进引号，整个脚本报语法错（改完记得用带 BOM 的方式存回去）。
param(
    [ValidateSet("arm64-v8a", "x86_64")]
    [string]$Abi = "arm64-v8a",
    [switch]$Release
)

$ErrorActionPreference = "Stop"

$root = Split-Path -Parent $PSScriptRoot
$androidDir = Join-Path $root "android"
$jniLibs = Join-Path $androidDir "app/src/main/jniLibs"

$target = switch ($Abi) {
    "arm64-v8a" { "aarch64-linux-android" }
    "x86_64" { "x86_64-linux-android" }
}

# cargo-ndk 认这三个环境变量里的任意一个；都没设就从 SDK 目录里挑一个装了的 NDK
if (-not ($env:ANDROID_NDK_HOME -or $env:ANDROID_NDK_ROOT -or $env:NDK_HOME)) {
    $sdk = $env:ANDROID_HOME
    if (-not $sdk) { $sdk = $env:ANDROID_SDK_ROOT }
    if (-not $sdk) {
        throw "找不到 Android SDK：设 ANDROID_HOME，或在 android/local.properties 里写 sdk.dir"
    }
    $ndkRoot = Join-Path $sdk "ndk"
    if (-not (Test-Path $ndkRoot)) { throw "找不到 NDK：$ndkRoot 不存在（用 sdkmanager 装 ndk;<版本>）" }
    $env:ANDROID_NDK_HOME = (Get-ChildItem $ndkRoot -Directory | Sort-Object Name | Select-Object -Last 1).FullName
    Write-Host "用 SDK 里的 NDK：$env:ANDROID_NDK_HOME" -ForegroundColor DarkGray
}

# 1) Rust：编 cdylib（`--lib` 只编库，不编桌面那个 bin），产物直接落到 jniLibs/<abi>/
$cargoArgs = @("ndk", "-t", $Abi, "-o", $jniLibs, "build", "--lib")
if ($Release) { $cargoArgs += "--release" }
Write-Host "> cargo $($cargoArgs -join ' ')" -ForegroundColor Cyan
& cargo @cargoArgs
if ($LASTEXITCODE -ne 0) { throw "cargo ndk 失败（exit $LASTEXITCODE）" }

$so = Join-Path $jniLibs "$Abi/libToDay.so"
if (-not (Test-Path $so)) { throw "没有拿到 $so：cargo ndk 的输出目录或 lib 名对不上" }
Write-Host ("libToDay.so  {0:N0} 字节" -f (Get-Item $so).Length) -ForegroundColor DarkGray

# 2) Gradle：把 .so 打进 APK（没有 Java/Kotlin 源码，这一步就是打包资源与 manifest）
$task = if ($Release) { "assembleRelease" } else { "assembleDebug" }
Write-Host "> gradlew $task" -ForegroundColor Cyan
Push-Location $androidDir
try {
    & ".\gradlew.bat" $task
    if ($LASTEXITCODE -ne 0) { throw "gradle 失败（exit $LASTEXITCODE）" }
} finally {
    Pop-Location
}

$apk = Join-Path $androidDir "app/build/outputs/apk/$(if ($Release) { 'release' } else { 'debug' })/app-$(if ($Release) { 'release' } else { 'debug' }).apk"
Write-Host "APK：$apk" -ForegroundColor Green
Write-Host "安装：adb install -r `"$apk`"" -ForegroundColor Green
