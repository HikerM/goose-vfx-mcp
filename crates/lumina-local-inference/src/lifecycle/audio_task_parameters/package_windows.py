"""Derive the pinned long-path-aware audio.cpp executable from the verified CPU build."""

import argparse
import hashlib
import os
from pathlib import Path
import shutil
import subprocess
import tempfile


SPEECH_UPSTREAM = "da25e095e147751e8c4db6634ef2fd7875ffe4883c8a70b2b01826cdb39ae787"
SPEECH_PACKAGED = "b319916421e54024ba4229462e1a577f368627176fc538d3be6d4a8a5398394f"
MUSIC_UPSTREAM = "350f7d1e03e7ea01592796a8989c8c60bd07a442bff2648ea0218e587b8fec7b"
MUSIC_PACKAGED = "5e53e8441457c990d11955873e513c29030e04c3d19229c62eeb03b0e118ad8b"
MANIFEST_TOOL = "1b8a272d586a9ab53ac2ccd457a88bd0210d7d7ac3daea5b34743cc2afe73b26"


def verify(path, digest):
    if hashlib.sha256(path.read_bytes()).hexdigest() != digest:
        raise ValueError(f"Unexpected SHA-256 for {path}")


def package(registry, mt, original=None):
    original = original or registry / SPEECH_UPSTREAM / "audiocpp_cli.exe"
    upstream = hashlib.sha256(original.read_bytes()).hexdigest()
    known = {SPEECH_UPSTREAM: SPEECH_PACKAGED, MUSIC_UPSTREAM: MUSIC_PACKAGED}
    if upstream not in known:
        raise ValueError(f"Unknown upstream distribution: {original}")
    packaged = known[upstream]
    verify(original, upstream)
    verify(mt, MANIFEST_TOOL)
    target = registry / packaged / original.name
    if target.exists():
        verify(target, packaged)
        return target
    target.parent.mkdir(exist_ok=True)
    with tempfile.TemporaryDirectory(dir=registry) as temporary:
        stage = Path(temporary)
        binary = stage / original.name
        manifest = stage / "application.manifest"
        shutil.copyfile(original, binary)
        verify(binary, upstream)
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
        verify(binary, packaged)
        try:
            os.rename(binary, target)
        except FileExistsError:
            verify(target, packaged)
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
    parser.add_argument("--original", type=Path, help="Verified upstream distribution executable")
    args = parser.parse_args()
    print(package(args.registry.resolve(strict=True), args.mt.resolve(strict=True), args.original))
