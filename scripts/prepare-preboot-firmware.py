#!/usr/bin/env python3
"""Host-only builder: preserve an original bundle and replace only ramdisk.img.

All outputs use exclusive creation. The only subprocess is the host-only
published arkforge artifact preboot-ramdisk command. No device commands exist.
"""
import argparse
import gzip
import hashlib
import io
import json
from pathlib import Path
import subprocess
import tarfile
from preboot_support import support_archive


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


class HashedReader:
    def __init__(self, stream):
        self.stream = stream
        self.hash = hashlib.sha256()

    def read(self, size=-1):
        value = self.stream.read(size)
        self.hash.update(value)
        return value


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--arkforge", type=Path, required=True)
    parser.add_argument("--runtime-dir", type=Path, required=True)
    parser.add_argument("--file", type=Path, required=True)
    parser.add_argument("--expect-sha256", required=True)
    parser.add_argument("--support-library", type=Path, required=True,
                        help="Pinned libcrypto_openssl.z.so extracted from the original system.img")
    parser.add_argument("--out-dir", type=Path, required=True)
    args = parser.parse_args()
    original = digest(args.file)
    if original != args.expect_sha256:
        raise ValueError("original archive SHA-256 differs from the supplied immutable reference")
    args.out_dir.mkdir()
    support = args.out_dir / "preboot-support.cpio.gz"
    with args.support_library.open("rb") as stream:
        library = stream.read(2745488 + 1)
    with support.open("xb") as output:
        output.write(support_archive(library))
    base = args.out_dir / "original-ramdisk.img"
    with tarfile.open(args.file, "r:gz") as source:
        members = source.getmembers()
        if any(not member.isfile() for member in members):
            raise ValueError("firmware contains a non-regular member")
        names = [member.name for member in members]
        if len(names) != len(set(names)) or names.count("ramdisk.img") != 1:
            raise ValueError("duplicate archive member or absent ramdisk")
        ramdisk = source.getmember("ramdisk.img")
        if ramdisk.size != 2373467:
            raise ValueError("ramdisk size differs from the pinned input")
        with source.extractfile(ramdisk) as stream, base.open("xb") as output:
            output.write(stream.read())
    derived = args.out_dir / "acceptance-ramdisk.img"
    restoration = args.out_dir / "restoration-ramdisk.img"
    process = subprocess.run(
        [str(args.arkforge.resolve()), "--runtime-dir", str(args.runtime_dir.resolve()),
         "--output", "json", "artifact", "preboot-ramdisk", "--file", str(base.resolve()),
         "--support-archive", str(support.resolve()),
         "--out", str(derived.resolve()), "--restore-out", str(restoration.resolve())], check=True, text=True, capture_output=True,
    )
    prepared = json.loads(process.stdout)
    if prepared["device_accessed"] or digest(derived) != prepared["ramdisk_sha256"]:
        raise ValueError("host derivative did not match its returned digest")
    if digest(restoration) != prepared["restoration_ramdisk_sha256"]:
        raise ValueError("restoration image did not match its pinned digest")
    bundles = []
    for name, image in [("acceptance", derived), ("restoration", restoration)]:
        rows = []
        firmware = args.out_dir / (name + "-firmware.tar.gz")
        with tarfile.open(args.file, "r|gz") as source, firmware.open("xb") as raw:
            with gzip.GzipFile(fileobj=raw, mode="wb", compresslevel=1, mtime=0) as compressed:
                with tarfile.open(fileobj=compressed, mode="w|", format=tarfile.USTAR_FORMAT) as output:
                    for member in source:
                        if member.name == "ramdisk.img":
                            payload = image.read_bytes()
                            member.size = len(payload)
                            reader = HashedReader(io.BytesIO(payload))
                        else:
                            reader = HashedReader(source.extractfile(member))
                        output.addfile(member, reader)
                        rows.append({"member": member.name, "size_bytes": member.size,
                                     "sha256": reader.hash.hexdigest(),
                                     "changed": member.name == "ramdisk.img"})
        bundles.append({"kind": name, "artifact_sha256": digest(firmware), "members": rows})
    if digest(args.file) != original:
        raise ValueError("original archive changed during derivation")
    report = {"schema": "arkforge.preboot-firmware/v1", "device_accessed": False,
              "source_artifact_sha256": original, "bundles": bundles, "ramdisk": prepared}
    with (args.out_dir / "provenance.json").open("x", encoding="utf-8") as output:
        json.dump(report, output, indent=2)
    print(json.dumps(report))


if __name__ == "__main__":
    main()
