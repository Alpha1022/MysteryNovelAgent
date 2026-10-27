"""故障注入测试：不下载或修改真实 Android SDK。"""
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

SCRIPT = Path(__file__).with_name("install-android-sdk.sh").resolve()
MANAGER = r'''#!/usr/bin/env bash
set -eu
if [[ "$*" == *--licenses* ]]; then exit "${LICENSE_EXIT:-0}"; fi
package="$3"
echo "$package" >> "$ANDROID_HOME/calls"
case "$package" in
  platform-tools) directory=platform-tools; marker=adb ;;
  'platforms;android-36') directory=platforms/android-36; marker=android.jar ;;
  'build-tools;'*) directory="build-tools/${package#*;}"; marker=aapt2 ;;
  'ndk;'*) directory="ndk/${package#*;}"; marker=toolchains/llvm/prebuilt/linux-x86_64/bin/clang ;;
  *) exit 8 ;;
esac
if [[ "$directory" == ndk/* ]]; then
  count=$(grep -c '^ndk;' "$ANDROID_HOME/calls")
  if [[ "${MODE:-}" == permanent || ( "${MODE:-}" == transient && "$count" == 1 ) ]]; then
    mkdir -p "$ANDROID_HOME/$directory" "$ANDROID_HOME/.temp"
    touch "$ANDROID_HOME/$directory/broken" "$ANDROID_HOME/.temp/broken.zip"
    echo 'Error on ZipFile unknown archive' >&2
    exit 1
  fi
  if [[ "${MODE:-}" == incomplete && "$count" == 1 ]]; then exit 0; fi
  [[ ! -e "$ANDROID_HOME/$directory/broken" && ! -e "$ANDROID_HOME/.temp/broken.zip" ]] || exit 9
  mkdir -p "$ANDROID_HOME/$directory/toolchains/llvm/prebuilt/linux-x86_64/sysroot"
fi
mkdir -p "$ANDROID_HOME/$directory/$(dirname "$marker")"
printf 'Pkg.Revision = %s\n' "$ANDROID_NDK_VERSION" > "$ANDROID_HOME/$directory/source.properties"
printf '#!/usr/bin/env bash\nexit 0\n' > "$ANDROID_HOME/$directory/$marker"
chmod +x "$ANDROID_HOME/$directory/$marker"
'''


class InstallTest(unittest.TestCase):
  def setUp(self):
    temp = tempfile.TemporaryDirectory()
    self.addCleanup(temp.cleanup)
    self.root = Path(temp.name)
    manager = self.root / "cmdline-tools/latest/bin/sdkmanager"
    manager.parent.mkdir(parents=True)
    manager.write_text(MANAGER)
    manager.chmod(0o755)
    bin_dir = self.root / "bin"
    bin_dir.mkdir()
    sleep = bin_dir / "sleep"
    sleep.write_text("#!/usr/bin/env bash\nexit 0\n")
    sleep.chmod(0o755)
    self.env = dict(os.environ, ANDROID_HOME=str(self.root), ANDROID_NDK_VERSION="26.3.11579264",
                    GITHUB_ENV=str(self.root / "env"), PATH=str(bin_dir) + os.pathsep + os.environ["PATH"])

  def run_script(self, **kwargs):
    return subprocess.run(["bash", str(SCRIPT)], env=dict(self.env, **kwargs),
                          text=True, capture_output=True, timeout=15)

  def test_corrupt_download_is_cleaned_and_valid_packages_are_reused(self):
    result = self.run_script(MODE="transient")
    self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
    calls = (self.root / "calls").read_text().splitlines()
    self.assertEqual(calls.count("ndk;26.3.11579264"), 2)
    self.assertEqual(calls.count("platform-tools"), 1)
    self.assertEqual(len((self.root / "env").read_text().splitlines()), 3)
    self.assertEqual(self.run_script().returncode, 0)
    self.assertEqual((self.root / "calls").read_text().splitlines(), calls)

  def test_incomplete_success_is_retried(self):
    result = self.run_script(MODE="incomplete")
    self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
    self.assertIn("reported success", result.stdout)

  def test_permanent_failure_does_not_export_ndk(self):
    result = self.run_script(MODE="permanent")
    self.assertNotEqual(result.returncode, 0)
    self.assertIn("after 3 attempts", result.stdout)
    self.assertFalse((self.root / "env").exists())

  def test_license_failure_is_not_swallowed(self):
    self.assertEqual(self.run_script(LICENSE_EXIT="7").returncode, 7)
    self.assertFalse((self.root / "calls").exists())

  def test_rejects_unsafe_cleanup_paths(self):
    self.assertNotEqual(self.run_script(ANDROID_HOME="/").returncode, 0)
    self.assertNotEqual(self.run_script(ANDROID_NDK_VERSION="../../other").returncode, 0)


if __name__ == "__main__":
  unittest.main()
