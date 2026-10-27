#!/usr/bin/env bash
# GitHub Linux runner：逐包安装，下载/解压失败后清理残留并重试。
set -euo pipefail

: "${ANDROID_HOME:?ANDROID_HOME is required}"
: "${ANDROID_NDK_VERSION:?ANDROID_NDK_VERSION is required}"
: "${GITHUB_ENV:?GITHUB_ENV is required}"
[[ "$ANDROID_HOME" = /* && "$ANDROID_HOME" != / ]] || {
  echo "::error::ANDROID_HOME must be an absolute SDK directory, not /"
  exit 1
}
[[ "$ANDROID_NDK_VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || {
  echo "::error::Invalid ANDROID_NDK_VERSION"
  exit 1
}
sdk_root=$(cd "$ANDROID_HOME" && pwd -P)
[[ "$sdk_root" != / ]] || exit 1
sdkmanager="$sdk_root/cmdline-tools/latest/bin/sdkmanager"

# 只忽略 yes 在 sdkmanager 提前关闭 stdin 时的 SIGPIPE，许可证命令本身失败仍退出。
set +e
yes 2>/dev/null | "$sdkmanager" --sdk_root="$sdk_root" --licenses >/dev/null
license_status=("${PIPESTATUS[@]}")
set -e
if (( license_status[1] != 0 )); then
  echo "::error::Android SDK license acceptance failed"
  exit "${license_status[1]}"
fi

package_ready() {
  local directory="$1" marker="$2"
  [[ -f "$sdk_root/$directory/source.properties" && -f "$sdk_root/$directory/$marker" ]] || return 1
  if [[ "$directory" == ndk/* ]]; then
    local revision
    revision=$(sed -n 's/^Pkg\.Revision[[:space:]]*=[[:space:]]*//p' "$sdk_root/$directory/source.properties" | tr -d '\r')
    [[ "$revision" == "$ANDROID_NDK_VERSION" ]] || return 1
    [[ -d "$sdk_root/$directory/toolchains/llvm/prebuilt/linux-x86_64/sysroot" ]] || return 1
    "$sdk_root/$directory/$marker" --version >/dev/null 2>&1 || return 1
  fi
}

install_package() {
  local package="$1" directory="$2" marker="$3" attempt
  # directory 仅来自下方固定列表；版本号已校验。删除范围限定在已解析的 SDK 根目录内。
  for attempt in 1 2 3; do
    if package_ready "$directory" "$marker"; then
      echo "Android SDK ready: $package"
      return 0
    fi
    rm -rf -- "$sdk_root/$directory" "$sdk_root/.temp"
    echo "::group::Install $package (attempt $attempt/3)"
    if "$sdkmanager" --sdk_root="$sdk_root" --install "$package" --verbose; then
      if package_ready "$directory" "$marker"; then
        echo "::endgroup::"
        return 0
      fi
      echo "::warning::sdkmanager reported success but $package is incomplete"
    fi
    echo "::endgroup::"
    echo "::warning::Installation failed for $package (attempt $attempt/3)"
    if (( attempt < 3 )); then sleep "$((attempt * 5))"; fi
  done
  echo "::error::Unable to install $package after 3 attempts"
  return 1
}

install_package 'platform-tools' 'platform-tools' 'adb'
install_package 'platforms;android-36' 'platforms/android-36' 'android.jar'
install_package 'build-tools;35.0.0' 'build-tools/35.0.0' 'aapt2'
install_package 'build-tools;36.0.0' 'build-tools/36.0.0' 'aapt2'
install_package "ndk;$ANDROID_NDK_VERSION" "ndk/$ANDROID_NDK_VERSION" 'toolchains/llvm/prebuilt/linux-x86_64/bin/clang'

ndk="$sdk_root/ndk/$ANDROID_NDK_VERSION"
printf 'ANDROID_NDK_HOME=%s\nANDROID_NDK_ROOT=%s\nNDK_HOME=%s\n' "$ndk" "$ndk" "$ndk" >> "$GITHUB_ENV"
