#!/usr/bin/env python3
"""Check a prepared project tree without printing suspected secret contents.

This is a developer check, not an application dependency or a guarantee that
every possible secret is detectable. Files are scanned as bytes, including
executables and photo metadata. It does not extract archives or inspect pixels.
Reports contain relative filenames, finding categories and counts only.

Usage: python scripts/check_privacy.py [project] [--deny-prefix PREFIX ...]
"""

import argparse
from collections import Counter
import contextlib
import errno
import getpass
import json
import os
from pathlib import Path
import re
import stat
import sys


CHUNK_SIZE = 1024 * 1024
TOKEN_PATTERNS = (
    ("github_token", rb"\bgh[pousr]_[A-Za-z0-9]{30,255}\b"),
    ("github_token", rb"\bgithub_pat_[A-Za-z0-9_]{40,255}\b"),
    ("cloud_access_key", rb"\b(?:AKIA|ASIA)[A-Z0-9]{16}\b"),
    ("service_token", rb"\bsk-(?:proj-|svcacct-)?[A-Za-z0-9_-]{32,255}\b"),
    ("service_token", rb"\bxox[baprs]-[A-Za-z0-9-]{20,255}\b"),
    ("service_token", rb"\bglpat-[A-Za-z0-9_-]{20,255}\b"),
    ("private_key", rb"-----BEGIN (?:[A-Z0-9]+ )?PRIVATE KEY-----"),
)


def path_variants(value):
    """Include native separators and JSON-escaped Windows paths."""
    value = str(value).rstrip("/\\")
    if not value or value in (os.path.sep, "."):
        return set()
    variants = {value, value.replace("\\", "/"), value.replace("/", "\\")}
    return variants | {item.replace("\\", "\\\\") for item in variants}


def rules_for(root, extra_prefixes):
    prefixes = {str(root)}
    # The parent also catches the old workspace before a project was moved into
    # its own subdirectory. Do not treat a filesystem root as identifying data.
    if len(root.parent.parts) > 2:
        prefixes.add(str(root.parent))
    try:
        home = Path.home()
        if len(home.parts) > 1:
            prefixes.add(str(home))
    except RuntimeError:
        pass
    prefixes.update(extra_prefixes)
    for name in ("CARGO_HOME", "RUSTUP_HOME", "VIRTUAL_ENV", "CONDA_PREFIX"):
        value = os.environ.get(name)
        if value and Path(value).is_absolute():
            prefixes.add(value)

    rules = [(category, re.compile(pattern)) for category, pattern in TOKEN_PATTERNS]
    # Literal prefixes also detect UTF-16 strings in Windows binaries/metadata.
    literals = set()
    for prefix in prefixes:
        for value in path_variants(prefix):
            for encoding in ("utf-8", "utf-16-le", "utf-16-be"):
                literals.add(value.encode(encoding, errors="surrogatepass"))
    for literal in sorted(literals):
        rules.append(("local_build_path", re.compile(re.escape(literal))))

    # Service-specific keys do not all have recognizable public prefixes. Use
    # secret-looking environment variables as an additional in-memory denylist;
    # their names and values are never copied into the report or source tree.
    secret_name = re.compile(r"(?:^|_)(?:TOKEN|API_KEY|SECRET|PASSWORD|SECRET_ACCESS_KEY)$")
    for name, value in os.environ.items():
        if secret_name.search(name.upper()) and len(value) >= 12:
            for encoding in ("utf-8", "utf-16-le", "utf-16-be"):
                literal = value.encode(encoding, errors="surrogatepass")
                literals.add(literal)
                rules.append(("configured_secret", re.compile(re.escape(literal))))

    # Match the account name in paths and identity fields rather than arbitrary
    # short substrings of compressed photos or third-party attribution text.
    try:
        username = getpass.getuser()
    except (KeyError, OSError):
        username = ""
    if username and username.lower() not in ("root", "runner", "user", "admin"):
        name = re.escape(username.encode("utf-8"))
        rules.append(("local_account", re.compile(
            rb"(?i)(?:[/\\]|\b(?:user(?:name)?|owner|author)[\"']?\s*[:=]\s*[\"']?)"
            + name + rb"(?:[/\\@]|\b)")))
    overlap = max([1024] + [len(item) + 16 for item in literals])
    return rules, overlap


def safe_relative(path, root, rules):
    relative = path.relative_to(root).as_posix()
    # A filename can itself be a credential. Do not echo it into a new report.
    raw = relative.encode("utf-8", errors="surrogatepass")
    if any(pattern.search(raw) for category, pattern in rules
           if category in ("github_token", "cloud_access_key", "service_token", "private_key", "configured_secret")):
        return "[sensitive filename withheld]"
    return relative


def protected_filename(path):
    name = path.name.lower()
    if any(name.endswith(suffix) for suffix in (".example", ".sample", ".template", ".dist")):
        return False
    if name == ".env" or (name.startswith(".env.") and name not in (".env.example", ".env.sample")):
        return True
    if name in ("id_rsa", "id_dsa", "id_ecdsa", "id_ed25519", ".netrc", "credentials", "credentials.json"):
        return True
    return name.endswith((".p12", ".pfx"))


@contextlib.contextmanager
def preserved_file(path):
    """Avoid access-time changes, or restore through the same open file handle.

    O_NOATIME is preferred on Linux. On systems without it, the file must permit
    timestamp updates before its contents are read. Unsupported/denied cases are
    reported as incomplete scans instead of silently changing photo timestamps.
    """
    flags = os.O_RDONLY | getattr(os, "O_BINARY", 0) | getattr(os, "O_NOFOLLOW", 0)
    noatime = getattr(os, "O_NOATIME", 0)
    protected = bool(noatime)
    try:
        fd = os.open(path, flags | noatime)
    except OSError as error:
        if not noatime or error.errno not in (errno.EPERM, errno.EACCES, errno.EINVAL, errno.ENOTSUP):
            raise
        fd = os.open(path, flags)
        protected = False
    before = os.fstat(fd)
    try:
        if not stat.S_ISREG(before.st_mode):
            raise OSError("not a regular file")
        if not protected:
            # Fail before reading if restoring through this handle is not
            # supported or permitted. This does not alter the requested times.
            os.utime(fd, ns=(before.st_atime_ns, before.st_mtime_ns))
        with os.fdopen(fd, "rb", closefd=False) as source:
            yield source
    finally:
        try:
            after = os.fstat(fd)
            if (before.st_dev, before.st_ino, before.st_size, before.st_mtime_ns) != (
                    after.st_dev, after.st_ino, after.st_size, after.st_mtime_ns):
                raise OSError("file changed during privacy scan")
            if after.st_atime_ns != before.st_atime_ns:
                os.utime(fd, ns=(before.st_atime_ns, after.st_mtime_ns))
                restored = os.fstat(fd)
                if (restored.st_atime_ns, restored.st_mtime_ns) != (before.st_atime_ns, after.st_mtime_ns):
                    raise OSError("file times could not be restored exactly")
        finally:
            os.close(fd)


def scan_bytes(source, rules, overlap):
    counts = Counter()
    seen = set()
    previous = b""
    consumed = 0
    while True:
        chunk = source.read(CHUNK_SIZE)
        if not chunk:
            break
        buffer = previous + chunk
        base = consumed - len(previous)
        for category, pattern in rules:
            for match in pattern.finditer(buffer):
                identity = category, base + match.start()
                if identity not in seen:
                    seen.add(identity)
                    counts[category] += 1
        consumed += len(chunk)
        previous = buffer[-overlap:]
        # Only offsets still in the overlap can be encountered a second time.
        cutoff = consumed - len(previous)
        seen = {identity for identity in seen if identity[1] >= cutoff}
    return counts, consumed


def scan(root, extra_prefixes):
    rules, overlap = rules_for(root, extra_prefixes)
    findings = []
    errors = []
    files = size = 0

    def report_error(path, category):
        errors.append({"path": safe_relative(path, root, rules), "category": category})

    def walk_error(error):
        # Exception strings may contain a full private path; never serialize it.
        path = Path(error.filename) if error.filename else root
        report_error(path, "directory_unreadable")

    for current, directories, filenames in os.walk(root, followlinks=False, onerror=walk_error):
        directory = Path(current)
        for name in list(directories) + filenames:
            path = directory / name
            if path.is_symlink():
                if name in directories:
                    directories.remove(name)
                try:
                    destination = path.resolve(strict=True)
                    internal = os.path.commonpath((str(root), str(destination))) == str(root)
                except (OSError, RuntimeError, ValueError):
                    internal = False
                report_error(path, "symlink_not_scanned" if internal else "external_or_broken_symlink")
                continue
            if name in directories:
                continue
            relative = safe_relative(path, root, rules)
            counts = Counter()
            if protected_filename(path):
                counts["credential_file"] += 1
            try:
                with preserved_file(path) as source:
                    matched, byte_count = scan_bytes(source, rules, overlap)
                counts.update(matched)
                files += 1
                size += byte_count
            except (OSError, ValueError, NotImplementedError):
                report_error(path, "file_unreadable_or_time_restore_failed")
            if counts:
                findings.append({"path": relative, "categories": dict(sorted(counts.items()))})
    return {
        "ok": not findings and not errors,
        "files_scanned": files,
        "bytes_scanned": size,
        "findings": findings,
        "incomplete": errors,
        "scope": "File contents and names; archives are not extracted and image pixels are not inspected.",
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("project", nargs="?", type=Path, default=Path(__file__).resolve().parents[1])
    parser.add_argument("--deny-prefix", action="append", default=[], metavar="PREFIX")
    args = parser.parse_args()
    root = args.project.resolve()
    if not root.is_dir():
        print(json.dumps({"ok": False, "incomplete": [{"category": "project_directory_missing"}]}))
        return 2
    report = scan(root, args.deny_prefix)
    print(json.dumps(report, ensure_ascii=False, indent=2))
    return 0 if report["ok"] else 1


if __name__ == "__main__":
    sys.exit(main())
