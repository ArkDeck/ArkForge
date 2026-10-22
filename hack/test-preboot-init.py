#!/usr/bin/env python3
"""Run the complete preboot wrapper with host-only applet fixtures.

All device paths are redirected into a temporary directory. Applets never mount,
read hardware, or start a long-running holder. This checks the wrapper's visible
handoff and filesystem effects; it is not a firmware or device acceptance test.
"""
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


SOURCE = Path(__file__).resolve().parents[1] / "crates/arkforge-artifact/src/preboot-init.sh"
ARGV_HEX = "2f 61 66 2d 70 72 65 62 6f 6f 74 2f 74 6f 79 62 6f 78 00 73 6c 65 65 70 00 36 30 30 00"
TOYBOX = r'''#!/bin/sh
applet=$1
shift
case "$applet" in
    mkdir) exec mkdir "$@" ;;
    chmod) exit 0 ;; # The production wrapper runs as root; fixtures need no privilege.
    ln) cp "$2" "$3"; chmod +x "$3" ;;
    timeout)
        test "$1 $2 $3" = '-s KILL 300' || exit 90
        shift 3
        if test "$AF_TEST_MEASUREMENT" = timeout; then exit 137; fi
        exec "$@" ;;
    sleep|umount) exit 0 ;;
    od)
        if test "$AF_TEST_READINESS" = missing; then
            printf 'READINESS_PROBE_FAILED\n' >&2
            exit 1
        fi
        printf '%s\n' "$AF_TEST_ARGV_HEX" ;;
    *) printf 'Unexpected fixture applet: %s\n' "$applet" >&2; exit 91 ;;
esac
'''
MEASUREMENT = r'''#!/bin/sh
printf 'MEASUREMENT_DIAGNOSTIC\n' >&2
if test "$AF_TEST_MEASUREMENT" = failed; then exit 2; fi
printf 'HOST_SYNTHETIC_REPORT\n' >root/af-preboot/report
'''
STOCK_INIT = r'''#!/bin/sh
# The pinned stock init tests existence, not file type, before mounting /dev.
if test -e root/dev/null; then
    printf 'STOCK_INIT_WOULD_SKIP_DEV_MOUNT\n' >&2
    exit 65
fi
test "$#" = 1 && test "$1" = handoff-argument || exit 66
printf 'STOCK_INIT_REACHED %s\n' "$(umask)"
'''


class PrebootInitTests(unittest.TestCase):
    def run_wrapper(self, measurement="success", readiness="ready", source=None):
        shell = shutil.which("sh")
        self.assertIsNotNone(shell, "These tests require the same POSIX shell as the compatibility suite")
        with tempfile.TemporaryDirectory(prefix="arkforge-preboot-init-") as directory:
            root = Path(directory) / "root"
            for name in ("bin", "dev", "proc", "sys"):
                (root / name).mkdir(parents=True)
            wrapper = SOURCE.read_text() if source is None else source
            # Relative fixture paths also work with the Windows Git shell.
            for path in ("/bin/init_early", "/bin/toybox", "/arkforge-preboot",
                         "/af-preboot", "/dev/null", "/proc", "/sys"):
                wrapper = wrapper.replace(path, "root" + path)
            wrapper = wrapper.replace("/bin/sh", "sh")
            for name, content in (("init", wrapper), ("bin/toybox", TOYBOX),
                                  ("arkforge-preboot", MEASUREMENT), ("bin/init_early", STOCK_INIT)):
                path = root / name
                path.write_text(content, newline="\n")
                path.chmod(0o755)
            env = dict(os.environ, AF_TEST_MEASUREMENT=measurement,
                       AF_TEST_READINESS=readiness, AF_TEST_ARGV_HEX=ARGV_HEX)
            result = subprocess.run([shell, "-c", "umask 022; exec sh root/init handoff-argument"],
                                    cwd=directory, env=env, capture_output=True, text=True, timeout=10)
            return result, (root / "af-preboot/report").read_text(), (root / "af-preboot/measure.log").read_text()

    def check_handoff(self, result):
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertRegex(result.stdout, r"STOCK_INIT_REACHED 0*22\n")
        self.assertNotIn("READINESS_PROBE_FAILED", result.stderr)

    def test_success_preserves_report_and_stock_dev_mount(self):
        result, report, log = self.run_wrapper()
        self.check_handoff(result)
        self.assertEqual(report, "HOST_SYNTHETIC_REPORT\n")
        self.assertIn("MEASUREMENT_DIAGNOSTIC", log)

    def test_readiness_errors_stay_in_ram_and_still_handoff(self):
        result, report, log = self.run_wrapper(readiness="missing")
        self.check_handoff(result)
        self.assertEqual(report, "HOST_SYNTHETIC_REPORT\n")
        self.assertEqual(log.count("READINESS_PROBE_FAILED"), 5)
        self.assertIn("MEASUREMENT_DIAGNOSTIC", log)

    def test_measurement_failure_and_timeout_remain_failures(self):
        for measurement, code in (("failed", 2), ("timeout", 137)):
            with self.subTest(measurement=measurement):
                result, report, _ = self.run_wrapper(measurement=measurement)
                self.check_handoff(result)
                self.assertEqual(report, f"ARKFORGE_PREBOOT_FAILED_V1 {code}\n")


if __name__ == "__main__":
    unittest.main()
