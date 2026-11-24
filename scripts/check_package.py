#!/usr/bin/env python3
"""Offline package-boundary regression with stub compiler/vendor tools.

Developer test only. Real tar/CMake exercise both Unix and Windows branches;
this does not claim to validate a Windows executable or replace native CI.
"""
import hashlib
import os
from pathlib import Path
import shutil
import subprocess
import tarfile
import tempfile

ROOT = Path(__file__).resolve().parents[1]


def put(path, text, executable=False):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text, encoding="utf-8")
    if executable:
        path.chmod(0o755)


def verify_manifest(archive, root):
    manifest = archive.extractfile(root + "/SHA256SUMS").read().decode()
    listed = set()
    for line in manifest.splitlines():
        expected, relative = line.split("  ", 1)
        listed.add(relative)
        data = archive.extractfile(root + "/" + relative).read()
        assert hashlib.sha256(data).hexdigest() == expected, relative
    assert listed == {member.name[len(root) + 1:] for member in archive.getmembers()
                      if member.isfile() and member.name != root + "/SHA256SUMS"}
    return manifest


def main():
    for target in ["x86_64-unknown-linux-gnu", "x86_64-pc-windows-msvc"]:
        with tempfile.TemporaryDirectory(prefix="photopinfix-package-test-") as temporary:
            project = Path(temporary) / "project with spaces"
            scripts = project / "scripts"
            scripts.mkdir(parents=True)
            for name in ["package_release.sh", "package_checksums.cmake", "relocate_native_install.cmake"]:
                shutil.copyfile(ROOT / "scripts" / name, scripts / name)
            for relative in ["Cargo.toml", "Cargo.lock", "README.md", "THIRD_PARTY_NOTICES.md", "LICENSE", "src/main.rs",
                             "docs/DEVELOPMENT.md", "assets/fonts/OFL.txt", "tests/sample.rs",
                             "demo/README.md", "demo/expected.json", "demo/raw-sources/catalog.json"]:
                put(project / relative, "public fixture\n")
            put(project / "docs/images/example.png", "documentation illustration")
            put(project / ".env", "PRIVATE_FIXTURE_DO_NOT_PACKAGE")
            put(project / "demo/private-photo.jpg", "PRIVATE_FIXTURE_DO_NOT_PACKAGE")
            binary_name = "photopinfix.exe" if "windows" in target else "photopinfix"
            binary = project / binary_name
            put(binary, "#!/bin/sh\nprintf 'PhotoPinFix test\\n'\n", executable=True)
            native = project / "tools/native-heif"
            for lib in ["libheif", "libde265"]:
                put(native / "install/lib" / (lib + ".a"), "static library")
                put(native / "install/share/licenses" / (lib + "-COPYING"), "native notice")
                put(native / "sources" / lib / "COPYING", "native nested notice")
                put(native / "sources" / lib / "source.c", "native source")
                put(native / "downloads" / (lib + ".tar.gz"), "source archive")
            vcpkg = native / "vcpkg"
            put(vcpkg / ".vcpkg-root", "")
            for lib in ["libheif", "libde265"]:
                put(vcpkg / "installed/x64-windows-static/share" / lib / "copyright", "native windows notice")
                put(vcpkg / "installed/x64-windows-static/lib" / (lib + ".lib"), "static windows library")
                put(vcpkg / "buildtrees" / lib / "src/COPYING", "native windows source notice")
                put(vcpkg / "ports" / lib / "portfile.cmake", "port provenance")
            tools = Path(temporary) / "fake-tools"
            put(tools / "cargo", """#!/bin/sh
if [ "$1" = "-V" ]; then printf 'cargo test\\n'; exit 0; fi
[ "$1" = vendor ] || exit 1
mkdir -p vendor/test-crate-1.0/native
printf 'crate source\\n' > vendor/test-crate-1.0/lib.rs
printf 'crate notice\\n' > vendor/test-crate-1.0/LICENSE
printf 'nested notice\\n' > vendor/test-crate-1.0/native/COPYING
printf '[source.crates-io]\\nreplace-with = "vendored-sources"\\n[source.vendored-sources]\\ndirectory = "vendor"\\n'
""", executable=True)
            for name, body in {"rustc": "printf 'rustc test\\n'", "ldd": "printf 'system libraries only\\n'",
                               "dumpbin": "printf 'KERNEL32.dll\\n'", "git": "exit 1"}.items():
                put(tools / name, "#!/bin/sh\n" + body + "\n", executable=True)
            environment = dict(os.environ, PATH=str(tools) + os.pathsep + os.environ["PATH"],
                               PHOTOPINFIX_NATIVE_ROOT=str(native), VCPKG_ROOT=str(vcpkg),
                               VCPKGRS_TRIPLET="x64-windows-static", GITHUB_RUN_ID="12345")
            subprocess.run(["bash", str(scripts / "package_release.sh"), target, str(binary)],
                           cwd=project, env=environment, check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            output = project / "target/artifacts"
            app_name, source_name = f"photopinfix-{target}-app", f"photopinfix-{target}-source"
            source_path = output / (source_name + ".tar.gz")
            source_digest = hashlib.sha256(source_path.read_bytes()).hexdigest()
            for name in [app_name, source_name]:
                archive_path = output / (name + ".tar.gz")
                assert hashlib.sha256(archive_path.read_bytes()).hexdigest() in (output / (name + ".tar.gz.sha256")).read_text()
            with tarfile.open(output / (app_name + ".tar.gz")) as app, tarfile.open(source_path) as source:
                app_members, source_members = app.getnames(), source.getnames()
                assert app_name + "/" + binary_name in app_members
                assert not any("/source/" in name or "/vendor/" in name or name.endswith((".a", ".lib", ".rs")) for name in app_members)
                assert app_name + "/licenses/NotoSansSC/OFL.txt" in app_members
                assert app_name + "/licenses/rust/test-crate-1.0/native/COPYING" in app_members
                assert source_name + "/source/vendor/test-crate-1.0/lib.rs" in source_members
                assert source_name + "/source/docs/DEVELOPMENT.md" in source_members
                assert source_name + "/source/docs/images/example.png" in source_members
                assert source_name + "/source/demo/raw-sources/catalog.json" in source_members
                assert source_name + "/source/demo/expected.json" in source_members
                assert source_name + "/RELINKING.md" in source_members
                assert not any(name.endswith("/" + binary_name) for name in source_members)
                assert source_digest in app.extractfile(app_name + "/SOURCE.txt").read().decode()
                assert "./" + binary_name in app.extractfile(app_name + "/README.txt").read().decode()
                for archive, root in [(app, app_name), (source, source_name)]:
                    verify_manifest(archive, root)
                    for member in archive.getmembers():
                        assert member.uid == 0 and member.gid == 0
                        if member.isfile():
                            assert b"PRIVATE_FIXTURE_DO_NOT_PACKAGE" not in archive.extractfile(member).read()
            print("PASS", target, "paired app/source boundaries, notices, provenance and all checksums")


if __name__ == "__main__":
    main()
