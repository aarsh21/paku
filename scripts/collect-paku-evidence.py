#!/usr/bin/env python3
"""Bind verification artifacts to the working tree without copying ignored secrets/builds."""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import tarfile

ROOT = Path(__file__).resolve().parent.parent
OUT = Path(sys.argv[1]).resolve()
OUT.mkdir(parents=True, exist_ok=True)


def git(*args):
    return subprocess.check_output(["git", *args], cwd=ROOT)


def digest(path):
    h = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            h.update(block)
    return h.hexdigest()


def checksum_line(path, relative):
    # GNU sha256sum-compatible escaping, including unusual repository filenames.
    name = os.fspath(relative)
    escaped = "\\" in name or "\n" in name
    name = name.replace("\\", "\\\\").replace("\n", "\\n")
    return ("\\" if escaped else "") + digest(path) + "  " + name + "\n"


def source_paths(raw):
    result = []
    for name in raw.split(b"\0"):
        if not name:
            continue
        relative = Path(os.fsdecode(name))
        path = ROOT / relative
        if any(part in {"target", "node_modules", ".wrangler", "__pycache__"} for part in relative.parts):
            continue
        if path.resolve().is_relative_to(OUT):
            continue
        if path.is_file() or path.is_symlink():
            result.append(relative)
    return sorted(set(result), key=os.fspath)


sources = source_paths(git("ls-files", "--cached", "--others", "--exclude-standard", "-z"))
untracked = source_paths(git("ls-files", "--others", "--exclude-standard", "-z"))
(OUT / "git-status.txt").write_bytes(git("status", "--short"))
(OUT / "changes.patch").write_bytes(git(
    "-c", "diff.mnemonicPrefix=false", "-c", "diff.noPrefix=false",
    "-c", "diff.srcPrefix=a/", "-c", "diff.dstPrefix=b/", "-c", "color.ui=false",
    "diff", "HEAD", "--binary", "--no-ext-diff"))
with (OUT / "untracked-files.tar.gz").open("wb") as archive:
    with tarfile.open(fileobj=archive, mode="w:gz", dereference=False) as tar:
        for relative in untracked:
            tar.add(ROOT / relative, arcname=os.fspath(relative), recursive=False)
with (OUT / "source-sha256.txt").open("w") as hashes:
    for relative in sources:
        if (ROOT / relative).is_file():
            hashes.write(checksum_line(ROOT / relative, relative))
metadata = {
    "baseRevision": git("rev-parse", "HEAD").decode().strip(),
    "sourceFiles": len(sources), "untrackedFiles": len(untracked),
    "buildSettings": {key: os.environ.get(key) for key in [
        "CARGO_BUILD_JOBS", "PKG_CONFIG_PATH", "LD_LIBRARY_PATH", "LIBRARY_PATH",
        "WEBKIT_EXEC_PATH", "WEBKIT_INJECTED_BUNDLE_PATH", "RUST_MIN_STACK",
    ]},
    "reconstruction": "At baseRevision, apply changes.patch and extract untracked-files.tar.gz; source-sha256.txt binds the resulting files. Ignored files/build caches are excluded.",
}
(OUT / "source-metadata.json").write_text(json.dumps(metadata, indent=2) + "\n")
manifest = OUT / "SHA256SUMS"
with manifest.open("w") as hashes:
    for path in sorted(OUT.rglob("*")):
        if path.is_file() and path != manifest:
            hashes.write(checksum_line(path, path.relative_to(OUT)))
print(f"Bound {len(sources)} source files and verification artifacts: {manifest}")
