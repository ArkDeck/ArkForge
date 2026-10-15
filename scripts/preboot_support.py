"""Build the closed compressed dependency overlay from the original firmware."""
import gzip
import hashlib
import io

LIBRARY_SIZE = 2745488
LIBRARY_SHA256 = "4b9875e386e3d82f737cb496ef85f4199ad39e9bc12942b4bad804e423776844"
SUPPORT_SIZE = 1534719
SUPPORT_SHA256 = "e1d7bc7df36cf89e5c0c9640f1e502792c3ca39a14d67e48186028ad63b4fafe"


def support_archive(library):
    if len(library) != LIBRARY_SIZE or hashlib.sha256(library).hexdigest() != LIBRARY_SHA256:
        raise ValueError("preboot support library is not the pinned original firmware dependency")
    archive = bytearray()
    for inode, name, mode, data in [
        (1, "lib/chipset-sdk-sp/libcrypto_openssl.z.so", 0o100644, library),
        (2, "TRAILER!!!", 0, b""),
    ]:
        archive.extend(b"070701")
        for value in (inode, mode, 0, 0, 1, 0, len(data), 0, 0, 0, 0, len(name) + 1, 0):
            archive.extend(f"{value:08x}".encode())
        archive.extend(name.encode() + b"\0")
        archive.extend(b"\0" * (-len(archive) % 4))
        archive.extend(data)
        archive.extend(b"\0" * (-len(archive) % 4))
    output = io.BytesIO()
    with gzip.GzipFile(filename="", fileobj=output, mode="wb", compresslevel=9, mtime=0) as stream:
        stream.write(archive)
    result = output.getvalue()
    if len(result) != SUPPORT_SIZE or hashlib.sha256(result).hexdigest() != SUPPORT_SHA256:
        raise ValueError("compression output differs from the published support archive pin")
    return result
