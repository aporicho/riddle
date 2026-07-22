#!/usr/bin/env python3
"""Create or verify a ReMagic Store bundle manifest.

Canonical algorithms (all paths are UTF-8 POSIX paths, sorted bytewise):

* one file record is ``path NUL mode NUL size NUL sha256 LF``; ``mode`` is
  octal without a leading zero and ``size`` is base-10;
* ``payload_sha256`` is SHA-256 over the concatenated records for files below
  ``payload/``;
* ``content_id`` is SHA-256 over ``remagic-bundle-content-v1 NUL``, followed by
  ``app_id NUL package NUL version NUL`` and the records for every ordinary
  file except ``bundle.json``.

The JSON `files` array renders mode as a four-digit octal string. Symlinks,
devices, sockets and other special files are rejected before publication.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import stat
import sys
import tempfile
from typing import Any

BUNDLE_FILE = "bundle.json"
MANIFEST_FILE = "manifest.toml"
PAYLOAD_DIR = "payload"
CONTENT_DOMAIN = b"remagic-bundle-content-v1\0"
ALLOWED_MODES = {0o644, 0o755}


def file_hash(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def collect_files(root: Path) -> list[dict[str, Any]]:
    if not root.is_dir() or root.is_symlink():
        raise ValueError(f"bundle root is not a real directory: {root}")
    manifest = root / MANIFEST_FILE
    payload = root / PAYLOAD_DIR
    if not manifest.is_file() or manifest.is_symlink():
        raise ValueError(f"missing ordinary {MANIFEST_FILE}")
    if not payload.is_dir() or payload.is_symlink():
        raise ValueError(f"missing real {PAYLOAD_DIR}/ directory")

    files: list[dict[str, Any]] = []
    for path in root.rglob("*"):
        relative = path.relative_to(root).as_posix()
        info = path.lstat()
        if stat.S_ISDIR(info.st_mode):
            continue
        if relative == BUNDLE_FILE:
            if not stat.S_ISREG(info.st_mode):
                raise ValueError(f"{BUNDLE_FILE} is not an ordinary file")
            continue
        if not stat.S_ISREG(info.st_mode):
            raise ValueError(f"bundle contains a link or special file: {relative}")
        if info.st_nlink != 1:
            raise ValueError(f"bundle contains a hard-linked file: {relative}")
        if relative != MANIFEST_FILE and not relative.startswith(f"{PAYLOAD_DIR}/"):
            raise ValueError(f"unexpected bundle path: {relative}")
        if "\n" in relative or "\r" in relative or "\0" in relative:
            raise ValueError(f"unsafe bundle path: {relative!r}")
        mode = stat.S_IMODE(info.st_mode)
        if mode not in ALLOWED_MODES:
            raise ValueError(f"unsupported mode {mode:o} for {relative}")
        files.append(
            {
                "path": relative,
                "sha256": file_hash(path),
                "size": info.st_size,
                "mode": f"{mode:04o}",
            }
        )
    files.sort(key=lambda entry: entry["path"].encode("utf-8"))
    if not any(entry["path"].startswith(f"{PAYLOAD_DIR}/") for entry in files):
        raise ValueError("bundle payload is empty")
    return files


def record(entry: dict[str, Any]) -> bytes:
    mode = format(int(entry["mode"], 8), "o")
    return (
        entry["path"].encode("utf-8")
        + b"\0"
        + mode.encode("ascii")
        + b"\0"
        + str(entry["size"]).encode("ascii")
        + b"\0"
        + entry["sha256"].encode("ascii")
        + b"\n"
    )


def digest_records(files: list[dict[str, Any]]) -> str:
    digest = hashlib.sha256()
    for entry in files:
        digest.update(record(entry))
    return digest.hexdigest()


def bundle_document(root: Path, app_id: str, package: str, version: str) -> dict[str, Any]:
    files = collect_files(root)
    payload = [entry for entry in files if entry["path"].startswith(f"{PAYLOAD_DIR}/")]
    identity = hashlib.sha256()
    identity.update(CONTENT_DOMAIN)
    for value in (app_id, package, version):
        identity.update(value.encode("utf-8"))
        identity.update(b"\0")
    for entry in files:
        identity.update(record(entry))
    return {
        "schema": 1,
        "app_id": app_id,
        "package": package,
        "version": version,
        "content_id": identity.hexdigest(),
        "manifest_path": MANIFEST_FILE,
        "payload_sha256": digest_records(payload),
        "files": files,
    }


def write_document(root: Path, document: dict[str, Any]) -> None:
    descriptor, temporary = tempfile.mkstemp(prefix=".bundle.", dir=root)
    try:
        with os.fdopen(descriptor, "w", encoding="utf-8") as stream:
            json.dump(document, stream, ensure_ascii=False, indent=2, sort_keys=True)
            stream.write("\n")
            stream.flush()
            os.fsync(stream.fileno())
        os.chmod(temporary, 0o644)
        os.replace(temporary, root / BUNDLE_FILE)
    finally:
        try:
            os.unlink(temporary)
        except FileNotFoundError:
            pass


def validate_identity(value: str, label: str) -> str:
    if not value or any(character not in "abcdefghijklmnopqrstuvwxyz0123456789-." for character in value):
        raise ValueError(f"invalid {label}: {value!r}")
    return value


def run(action: str, root: Path, app_id: str, package: str, version: str) -> None:
    app_id = validate_identity(app_id, "app_id")
    package = validate_identity(package, "package")
    if not version or "\0" in version or "\n" in version or "\r" in version:
        raise ValueError(f"invalid version: {version!r}")
    expected = bundle_document(root, app_id, package, version)
    bundle_path = root / BUNDLE_FILE
    if action == "create":
        write_document(root, expected)
        return
    with bundle_path.open(encoding="utf-8") as stream:
        actual = json.load(stream)
    if actual != expected:
        raise ValueError(f"{BUNDLE_FILE} does not match bundle contents")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("action", choices=("create", "verify"))
    parser.add_argument("root", type=Path)
    parser.add_argument("--app-id", required=True)
    parser.add_argument("--package", required=True)
    parser.add_argument("--version", required=True)
    arguments = parser.parse_args()
    try:
        run(
            arguments.action,
            arguments.root,
            arguments.app_id,
            arguments.package,
            arguments.version,
        )
    except (OSError, ValueError, json.JSONDecodeError) as error:
        print(f"remagic-bundle: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
