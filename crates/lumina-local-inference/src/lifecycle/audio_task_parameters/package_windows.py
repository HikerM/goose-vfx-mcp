"""Derive the pinned long-path-aware audio.cpp executable from the verified CPU build."""

import argparse
import hashlib
import os
from pathlib import Path
import shutil
import subprocess
import tempfile


UPSTREAM = "da25e095e147751e8c4db6634ef2fd7875ffe4883c8a70b2b01826cdb39ae787"
PACKAGED = "b319916421e54024ba4229462e1a577f368627176fc538d3be6d4a8a5398394f"
MANIFEST_TOOL = "1b8a272d586a9ab53ac2ccd457a88bd0210d7d7ac3daea5b34743cc2afe73b26"


def verify(path, digest):
    if hashlib.sha256(path.read_bytes()).hexdigest() != digest:
        raise ValueError(f"Unexpected SHA-256 for {path}")


def package(registry, mt):
    original = registry / UPSTREAM / "audiocpp_cli.exe"
    verify(original, UPSTREAM)
    verify(mt, MANIFEST_TOOL)
    target = registry / PACKAGED / original.name
    if target.exists():
        verify(target, PACKAGED)
        return target
    target.parent.mkdir(exist_ok=True)
    with tempfile.TemporaryDirectory(dir=registry) as temporary:
        stage = Path(temporary)
        binary = stage / original.name
        manifest = stage / "application.manifest"
        shutil.copyfile(original, binary)
        verify(binary, UPSTREAM)
        subprocess.run(
            [mt, "-nologo", f"-inputresource:{binary};#1", f"-out:{manifest}"],
            check=True,
        )
        xml = manifest.read_bytes()
        end = b"</windowsSettings>"
        if xml.count(end) != 1:
            raise ValueError("Unexpected upstream application manifest")
        setting = b'<longPathAware xmlns="http://schemas.microsoft.com/SMI/2016/WindowsSettings">true</longPathAware>'
        manifest.write_bytes(xml.replace(end, setting + end))
        subprocess.run(
            [mt, "-nologo", "-manifest", manifest, f"-outputresource:{binary};#1"],
            check=True,
        )
        verify(binary, PACKAGED)
        try:
            os.rename(binary, target)
        except FileExistsError:
            verify(target, PACKAGED)
    return target


if __name__ == "__main__":
    if os.name != "nt":
        raise SystemExit("Windows is required")
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--registry",
        type=Path,
        required=True,
        help="Existing validated-binaries directory",
    )
    parser.add_argument(
        "--mt", type=Path, required=True, help="Windows SDK 10.0.26100 x64 mt.exe"
    )
    args = parser.parse_args()
    print(package(args.registry.resolve(strict=True), args.mt.resolve(strict=True)))
