#!/usr/bin/env python3
"""Stage and archive the Brickwave NextUI Tool Pak."""

from __future__ import annotations

import argparse
import shutil
import stat
import zipfile
from pathlib import Path


PACKAGE_VERSION = "00.2.0"


def copy_file(source: Path, destination: Path) -> None:
    destination.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(source, destination)


def add_to_zip(archive: zipfile.ZipFile, source: Path, relative: Path) -> None:
    info = zipfile.ZipInfo(relative.as_posix())
    info.create_system = 3
    executable = relative.name in {"launch.sh", "brickwave"}
    mode = stat.S_IFREG | (0o755 if executable else 0o644)
    info.external_attr = mode << 16
    info.compress_type = zipfile.ZIP_DEFLATED
    archive.writestr(info, source.read_bytes())


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()

    project = Path(__file__).resolve().parents[1]
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    package_name = f"Brickwave-NextUI-{PACKAGE_VERSION}"
    stage = output / package_name
    archive_path = output / f"{package_name}.zip"
    if stage.parent != output or archive_path.parent != output:
        raise SystemExit("unsafe output path")
    if stage.exists():
        shutil.rmtree(stage)
    if archive_path.exists():
        archive_path.unlink()

    pak = stage / "Tools" / "tg5040" / "Brickwave.pak"
    copy_file(project / "packaging" / "nextui" / "launch.sh", pak / "launch.sh")
    copy_file(project / "packaging" / "nextui" / "README.md", pak / "README.md")
    copy_file(project / "packaging" / "nextui" / "pak.json", pak / "pak.json")
    copy_file(project / "packaging" / "nextui" / "BUILD_INFO.md", pak / "BUILD_INFO.md")
    copy_file(project / "LICENSE", pak / "LICENSE")
    copy_file(project / "THIRD_PARTY_NOTICES.md", pak / "THIRD_PARTY_NOTICES.md")
    copy_file(
        project / "target" / "aarch64-unknown-linux-gnu" / "release" / "brickwave",
        pak / "bin" / "brickwave",
    )
    copy_file(
        project / "assets" / "brickwave.png",
        stage / "Tools" / "tg5040" / ".media" / "Brickwave.png",
    )

    with zipfile.ZipFile(archive_path, "w") as archive:
        for source in sorted(path for path in stage.rglob("*") if path.is_file()):
            add_to_zip(archive, source, source.relative_to(stage))

    print(stage)
    print(archive_path)


if __name__ == "__main__":
    main()
