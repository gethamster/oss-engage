#!/usr/bin/env python3
"""Package a checked native release and WASM inspector, including third-party notices."""
import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import tarfile

ROOT = Path(__file__).resolve().parents[1]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=ROOT / "target/release/engage")
    parser.add_argument("--output", type=Path, default=ROOT / ".cache/release")
    args = parser.parse_args()
    metadata = json.loads(subprocess.check_output(["cargo", "metadata", "--locked", "--format-version", "1"], cwd=ROOT))
    host = next(line.split(": ", 1)[1] for line in subprocess.check_output(["rustc", "-vV"], text=True).splitlines() if line.startswith("host: "))
    version = next(p["version"] for p in metadata["packages"] if p["name"] == "engage-cli")
    name = f"engage-{version}-{host}"
    directory = args.output / name
    if directory.exists():
        shutil.rmtree(directory)
    directory.mkdir(parents=True)
    if not args.binary.is_file():
        parser.error("Build the native release executable before packaging")
    assets = ROOT / "crates/engage-inspector/dist"
    if not (assets / "index.html").is_file():
        parser.error("Build the inspector before packaging")
    shutil.copy2(args.binary, directory / "engage")
    shutil.copytree(assets, directory / "inspector")
    for file in ["LICENSE", "NOTICE", "README.md", "PORTING.md"]:
        shutil.copy2(ROOT / file, directory / file)
    shutil.copytree(ROOT / "docs", directory / "docs")
    third_party = directory / "third-party"
    third_party.mkdir()
    inventory = []
    for package in metadata["packages"]:
        if package["source"] is None:
            continue
        source = Path(package["manifest_path"]).parent
        destination = third_party / f"{package['name']}-{package['version']}"
        copied = []
        for file in source.iterdir():
            if file.name.lower().startswith(("license", "copying", "notice")) and file.is_file() and not file.is_symlink():
                destination.mkdir(exist_ok=True)
                shutil.copy2(file, destination / file.name)
                copied.append(file.name)
        inventory.append({"name": package["name"], "version": package["version"], "license": package["license"], "repository": package.get("repository"), "notices": copied})
    (third_party / "inventory.json").write_text(json.dumps(inventory, indent=2) + "\n")
    try:
        revision = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True, stderr=subprocess.DEVNULL).strip()
    except subprocess.CalledProcessError:
        revision = os.environ.get("ENGAGE_BUILD_REVISION", "unversioned-source")
    (directory / "build.json").write_text(json.dumps({"version": version, "host": host, "revision": revision}, indent=2) + "\n")
    archive = args.output / f"{name}.tar.gz"
    with tarfile.open(archive, "w:gz") as output:
        output.add(directory, arcname=name)
    print(archive.resolve())


if __name__ == "__main__":
    main()
