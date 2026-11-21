#!/usr/bin/env python3
"""Install checksum-pinned official Linux CI tools without compiling them."""
import argparse
import hashlib
import os
from pathlib import Path
import platform
import shutil
import tarfile
import urllib.request

ROOT = Path(__file__).resolve().parents[1]
# Digests are the official release asset digests, verified through GitHub's API.
TOOLS = [
    ("trunk", "https://github.com/trunk-rs/trunk/releases/download/v0.22.2/trunk-x86_64-unknown-linux-gnu.tar.gz", "26fca8a9952e46cd7d8b8b9a2e2c1786c090fc14b8e10b6678ab4d0f9f1a385f"),
    ("cargo-audit", "https://github.com/rustsec/rustsec/releases/download/cargo-audit/v0.22.2/cargo-audit-x86_64-unknown-linux-gnu-v0.22.2.tgz", "ab28a1bdb54db4d5d8ad5981cf1f959410370b3d28250dbd35f6a44248620e39"),
]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--download-only", action="store_true", help="Verify/extract archives without putting Linux executables on PATH")
    args = parser.parse_args()
    if not args.download_only and (platform.system() != "Linux" or platform.machine() not in {"x86_64", "amd64"}):
        parser.error("These pinned binaries target Linux x86_64")
    folder = ROOT / ".cache/ci-tools"
    binary_dir = folder / "bin"
    binary_dir.mkdir(parents=True, exist_ok=True)
    for name, url, digest in TOOLS:
        archive = folder / (name + ".tgz")
        with urllib.request.urlopen(url, timeout=60) as response, archive.open("wb") as target:
            remaining = 128 * 1024 * 1024
            while chunk := response.read(min(1024 * 1024, remaining + 1)):
                remaining -= len(chunk)
                if remaining < 0:
                    raise RuntimeError("Tool archive exceeded its download bound")
                target.write(chunk)
        if hashlib.sha256(archive.read_bytes()).hexdigest() != digest:
            raise RuntimeError(f"Checksum mismatch for {name}")
        with tarfile.open(archive) as source:
            candidates = [member for member in source.getmembers() if member.isfile() and Path(member.name).name == name]
            if len(candidates) != 1:
                raise RuntimeError(f"Unexpected executable layout for {name}")
            # Copy exactly one regular member; never extract paths or links from an archive.
            data = source.extractfile(candidates[0])
            if data is None:
                raise RuntimeError(f"Missing executable for {name}")
            destination = binary_dir / name
            with destination.open("wb") as target:
                shutil.copyfileobj(data, target)
            destination.chmod(0o755)
        print(f"Verified pinned {name}")
    if not args.download_only:
        github_path = os.environ.get("GITHUB_PATH")
        if github_path:
            with Path(github_path).open("a") as target:
                target.write(str(binary_dir) + "\n")
        else:
            print(f"Add {binary_dir} to PATH")


if __name__ == "__main__":
    main()
