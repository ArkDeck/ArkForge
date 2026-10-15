#!/usr/bin/env python3
"""Exercise the complete report query against isolated host applet fixtures.

No device is accessed. Kernel filesystem numbers and display names are varied
independently to check RAM acceptance and persistent/unknown storage rejection.
"""
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


SOURCE = Path(__file__).resolve().parents[1] / "crates/arkforge-standalone/src/hdc_control/preboot.rs"
ARGV = b"/af-preboot/toybox\0sleep\x00600\0"
REPORT = "HOST_SYNTHETIC_REPORT\n"
TOYBOX = r'''#!/bin/sh
applet=$1
shift
case "$applet" in
    od|cat) exec "$applet" "$@" ;;
    readlink) printf '%s\n' "$AF_TEST_REPORT_PATH" ;;
    stat)
        case "$1 $2 $3" in
            '-L -c %u:%a') printf '%s\n' "$AF_TEST_OWNER_MODE" ;;
            '-L -c %s') printf '%s\n' "$AF_TEST_SIZE" ;;
            '-L -f -c')
                case "$4" in
                    '%t') printf '%s\n' "$AF_TEST_MAGIC" ;;
                    '%T') printf '%s\n' "$AF_TEST_FS_NAME" ;;
                    *) exit 90 ;;
                esac ;;
            *) exit 91 ;;
        esac ;;
    sha256sum) printf '%s  /sys/class/block/mmcblk0/device/cid\n' "$AF_TEST_CID" ;;
    *) printf 'Unexpected fixture applet: %s\n' "$applet" >&2; exit 92 ;;
esac
'''


class PrebootReportTests(unittest.TestCase):
    def run_query(self, magic="858458f6", display="unknown", owner_mode="0:400",
                  size="22", report_path="/af-preboot/report (deleted)",
                  holders=1, argv=ARGV, source=None, regular=True):
        shell = shutil.which("sh")
        self.assertIsNotNone(shell, "These tests require the same POSIX shell as the compatibility suite")
        content = SOURCE.read_text() if source is None else source
        query = content.split('const QUERY: &str = r#"', 1)[1].split('"#;', 1)[0]
        query = query.replace("/system/bin/toybox", "./toybox").replace("/proc/", "./proc/")
        with tempfile.TemporaryDirectory(prefix="arkforge-preboot-report-") as directory:
            root = Path(directory)
            boot = root / "proc/sys/kernel/random/boot_id"
            boot.parent.mkdir(parents=True)
            boot.write_text("01234567-89ab-cdef-0123-456789abcdef\n")
            for i in range(holders):
                holder = root / "proc" / str(197 + i)
                (holder / "fd").mkdir(parents=True)
                (holder / "cmdline").write_bytes(argv)
                if regular:
                    (holder / "fd/9").write_text(REPORT)
                else:
                    (holder / "fd/9").mkdir()
            for name, value in (("toybox", TOYBOX), ("query", query)):
                path = root / name
                path.write_text(value, newline="\n")
                path.chmod(0o755)
            env = dict(os.environ, AF_TEST_MAGIC=magic, AF_TEST_FS_NAME=display,
                       AF_TEST_OWNER_MODE=owner_mode, AF_TEST_SIZE=size,
                       AF_TEST_REPORT_PATH=report_path, AF_TEST_CID="ab" * 32)
            return subprocess.run([shell, "query"], cwd=directory, env=env,
                                  capture_output=True, text=True, timeout=10)

    def assert_accepted(self, result):
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.splitlines(), [
            "ARKFORGE_PREBOOT_READ_V1", "01234567-89ab-cdef-0123-456789abcdef",
            "ab" * 32 + "  /sys/class/block/mmcblk0/device/cid",
            "ARKFORGE_PREBOOT_RAM_V1", REPORT.strip(), "", "ARKFORGE_PREBOOT_READ_OK",
        ])

    def assert_rejected(self, result):
        self.assertNotEqual(result.returncode, 0)
        self.assertNotIn("ARKFORGE_PREBOOT_RAM_V1", result.stdout)
        self.assertNotIn(REPORT.strip(), result.stdout)
        self.assertNotIn("ARKFORGE_PREBOOT_READ_OK", result.stdout)

    def test_ramfs_does_not_depend_on_toybox_display_name(self):
        self.assert_accepted(self.run_query())

    def test_tmpfs_and_live_ram_path_are_accepted(self):
        self.assert_accepted(self.run_query(magic="1021994", display="tmpfs",
                                            report_path="/af-preboot/report"))

    def test_persistent_and_unknown_filesystems_are_refused_even_with_ram_name(self):
        for magic in ("f2f52010", "ef53", "794c7630", "unknown", "", "858458f6 extra"):
            with self.subTest(magic=magic):
                self.assert_rejected(self.run_query(magic=magic, display="ramfs"))

    def test_holder_and_file_guards_still_fail_closed(self):
        cases = [
            {"holders": 0}, {"holders": 2}, {"argv": ARGV + b"extra\0"},
            {"owner_mode": "1000:400"}, {"owner_mode": "0:600"},
            {"size": "0"}, {"size": "513"}, {"regular": False},
            {"report_path": "/data/report"},
        ]
        for case in cases:
            with self.subTest(case=case):
                self.assert_rejected(self.run_query(**case))


if __name__ == "__main__":
    unittest.main()
