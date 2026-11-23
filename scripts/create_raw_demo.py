#!/usr/bin/env python3
"""Download genuine CC0 camera RAWs and create clearly labelled GPS fixtures.

Original files are SHA256-pinned and never modified. Only generated copies get
simulated dates/GPS. No whole-file metadata stripping or RAW re-encoding occurs.
This is a development fixture generator, not an application runtime dependency.
"""

from __future__ import annotations

import argparse
from concurrent.futures import ThreadPoolExecutor
from datetime import datetime, timedelta, timezone
import hashlib
import io
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import time
from urllib.parse import quote
from urllib.request import Request, urlopen


PROJECT = Path(__file__).resolve().parents[1]
CATALOG = PROJECT / "demo/raw-sources/catalog.json"
SIGNATURE = "PhotoPinFix genuine RAW simulated GPS fixtures v1"
MAX_DOWNLOAD_BYTES = 64 * 1024 * 1024
FIXTURE_TIMESTAMP_NS = int(datetime(2020, 1, 2, 3, 4, 5, tzinfo=timezone.utc).timestamp()) * 10**9
sys.path.insert(0, str(PROJECT / "tools/preview-deps"))


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def json_write(path: Path, value: object) -> None:
    path.write_text(json.dumps(value, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")


def download(source: dict, directory: Path) -> dict:
    path = directory / source["file"]
    record = dict(source)
    if path.is_file() and sha256_file(path) == source["sha256"]:
        record.update(bytes=path.stat().st_size, local_file=f"raw-sources/{path.name}")
        print(f"Verified cached original: {path.name}", flush=True)
        return record
    last_error = None
    for attempt in range(3):
        temporary = path.with_name(path.name + ".part")
        try:
            request = Request(quote(source["source_url"], safe=":/?=&"), headers={"User-Agent": "PhotoPinFix-RAW-fixture-generator/0.1"})
            digest, size = hashlib.sha256(), 0
            with urlopen(request, timeout=90) as response, temporary.open("wb") as output:
                resolved_url = response.geturl()
                while chunk := response.read(1024 * 1024):
                    size += len(chunk)
                    if size > MAX_DOWNLOAD_BYTES:
                        raise ValueError("RAW exceeded the 64 MiB per-file download limit")
                    digest.update(chunk)
                    output.write(chunk)
            if digest.hexdigest() != source["sha256"]:
                raise ValueError(f"SHA256 mismatch for complete original {source['file']}")
            temporary.replace(path)
            record.update(bytes=size, resolved_url=resolved_url, local_file=f"raw-sources/{path.name}")
            print(f"Downloaded and verified {path.name}: {size:,} bytes", flush=True)
            return record
        except Exception as error:
            temporary.unlink(missing_ok=True)
            last_error = error
            print(f"Download attempt {attempt + 1}/3 failed for {path.name}: {error}", flush=True)
            if attempt < 2:
                time.sleep(attempt + 1)
    raise RuntimeError(f"Cannot download {source['source_url']}: {last_error}")


def tool_command(path: Path) -> list[str]:
    if path.suffix.lower() == ".exe":
        return [str(path)]
    perl = shutil.which("perl")
    if perl is None:
        raise RuntimeError("Perl not found; pass --exiftool with an ExifTool executable")
    return [perl, str(path)]


def run_tool(command: list[str], arguments: list[str], *, binary: bool = False) -> bytes:
    process = subprocess.run(command + arguments, stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=60)
    if process.returncode:
        raise RuntimeError(process.stderr.decode(errors="replace").strip() or process.stdout.decode(errors="replace").strip())
    if not binary and process.stderr:
        print(process.stderr.decode(errors="replace").strip(), flush=True)
    return process.stdout


def metadata(command: list[str], path: Path) -> dict:
    rows = json.loads(run_tool(command, ["-j", "-n", "-FileType", "-Make", "-Model", "-Artist", "-OwnerName", "-Copyright", "-DateTimeOriginal", "-GPSLatitude", "-GPSLongitude", "-ImageWidth", "-ImageHeight", str(path)]))
    record = rows[0]
    # ExifTool reports the input's full path; fixture records are portable and
    # must not preserve the developer's account or checkout location.
    record["SourceFile"] = path.name
    return record


def embedded_preview(command: list[str], path: Path) -> tuple[bytes, dict]:
    from PIL import Image
    for tag in ("JpgFromRaw", "PreviewImage", "OtherImage", "ThumbnailImage"):
        data = run_tool(command, ["-b", "-" + tag, str(path)], binary=True)
        if not data:
            continue
        try:
            with Image.open(io.BytesIO(data)) as image:
                details = {"tag": tag, "bytes": len(data), "format": image.format, "width": image.width, "height": image.height}
                image.verify()
            return data, details
        except Exception:
            continue
    if path.suffix.lower() == ".dng":
        # Leica M8 stores a rendered, uncompressed RGB thumbnail in IFD0 and
        # the untouched CFA sensor data in SubIFD. It is a genuine DNG preview,
        # not a JPEG blob; never interpret an arbitrary CFA IFD as a photo.
        with Image.open(path) as image:
            reduced = bool(image.tag_v2.get(254, 0) & 1)
            if reduced and image.tag_v2.get(262) == 2 and image.mode == "RGB":
                output = io.BytesIO()
                image.save(output, "PNG")
                data = output.getvalue()
                return data, {"tag": "DNG-IFD0-RGB", "bytes": len(data), "format": "PNG", "width": image.width, "height": image.height}
    raise RuntimeError(f"No decodable embedded preview in complete RAW {path.name}")


def coords(pair):
    return None if pair is None else {"lat": round(pair[0], 9), "lon": round(pair[1], 9)}


def fixture_specs(sources: list[dict]) -> list[dict]:
    specs = []
    by_type = {source["file_type"]: source for source in sources}
    a, b = (31.2304, 121.4737), (31.2394, 121.4917)
    c, d, e = (35.6812, 139.7671), (48.8566, 2.3522), (48.8606, 2.3602)

    def add(session, step, file_type, *, gps=None, expected=None, method=None, label="missing"):
        source = by_type["ARW" if file_type == "JPEG" else file_type]
        captured = datetime(2026, 3, 3, 7 + session * 2) + timedelta(seconds=step * 30)
        extension = "jpg" if file_type == "JPEG" else file_type
        filename = f"s{session}_{step:02d}_{source['make'].lower().replace(' ', '_')}_{label}.{extension}"
        specs.append({"file": filename, "make": source["make"], "model": source["model"], "file_type": file_type, "session": session, "captured_at": captured.isoformat(), "gps": coords(gps), "expected_coordinates": coords(expected), "expected_method": method, "case": "existing-gps" if gps else "raw-time-interpolation" if method == "time-interpolation" else "raw-copy-next", "source_id": source["id"], "source_file": source["file"]})

    add(1, 0, "JPEG", gps=a, label="anchor_start")
    for step, file_type in enumerate(("ARW", "CR2", "CR3"), 1):
        location = tuple(left + (right - left) * step / 4 for left, right in zip(a, b))
        add(1, step, file_type, expected=location, method="time-interpolation")
    add(1, 4, "JPEG", gps=b, label="anchor_end")
    for step, file_type in enumerate(("DNG", "NEF", "RAF")):
        add(2, step, file_type, expected=c, method="copy-next")
    add(2, 3, "JPEG", gps=c, label="anchor_end")
    add(3, 0, "JPEG", gps=d, label="anchor_start")
    for step, file_type in enumerate(("RW2", "ORF", "PEF"), 1):
        location = tuple(left + (right - left) * step / 4 for left, right in zip(d, e))
        add(3, step, file_type, expected=location, method="time-interpolation")
    add(3, 4, "ARW", gps=e, label="existing_gps_anchor")
    return specs


def write_simulated_metadata(command: list[str], path: Path, spec: dict) -> None:
    captured = datetime.fromisoformat(spec["captured_at"]).strftime("%Y:%m:%d %H:%M:%S")
    # GPS only: retaining Make/Model, MakerNotes, copyright, sensor data, previews.
    arguments = ["-overwrite_original", "-GPS:all=", "-XMP-exif:GPSLatitude=", "-XMP-exif:GPSLongitude=", "-XMP-exif:GPSAltitude=", "-XMP-exif:GPSAltitudeRef=", "-XMP-PhotoPinFix:all=", f"-EXIF:DateTimeOriginal={captured}", f"-EXIF:CreateDate={captured}", f"-XMP-exif:DateTimeOriginal={captured}", "-EXIF:SubSecTimeOriginal=", "-EXIF:SubSecTimeDigitized=", "-EXIF:OffsetTimeOriginal=", "-EXIF:OffsetTimeDigitized="]
    if spec["gps"] is not None:
        lat, lon = spec["gps"]["lat"], spec["gps"]["lon"]
        arguments += [f"-GPSLatitude={abs(lat):.9f}", f"-GPSLatitudeRef={'S' if lat < 0 else 'N'}", f"-GPSLongitude={abs(lon):.9f}", f"-GPSLongitudeRef={'W' if lon < 0 else 'E'}", "-GPSMapDatum=WGS-84"]
    run_tool(command, arguments + [str(path)])


def build(args, sources: list[dict]) -> None:
    output = args.output.resolve()
    marker = output / "manifest.json"
    if output.exists():
        if not args.reset:
            raise RuntimeError(f"{output} already exists; use --reset to rebuild generated fixtures")
        if not marker.is_file() or json.loads(marker.read_text()).get("signature") != SIGNATURE:
            raise RuntimeError("Refusing to replace a directory without this generator's fixture manifest")
    command = tool_command(args.exiftool.resolve()) + ["-config", str(PROJECT / "assets/photopinfix-exiftool.config")]
    # Fail early on missing dependencies, before downloading dozens of megabytes.
    from PIL import Image
    run_tool(command, ["-ver"])
    cache = PROJECT / "demo/raw-sources"
    cache.mkdir(parents=True, exist_ok=True)
    with ThreadPoolExecutor(max_workers=2) as pool:
        records = list(pool.map(lambda source: download(source, cache), sources))
    preview_data = {}
    for record in records:
        path = cache / record["file"]
        tags = metadata(command, path)
        if tags.get("FileType", "").upper() != record["file_type"]:
            raise RuntimeError(f"Original type mismatch: {path.name} is {tags.get('FileType')}")
        data, preview = embedded_preview(command, path)
        preview_data[record["id"]] = data
        record["embedded_preview"] = preview
        tags["SourceFile"] = record["local_file"]
        record["original_metadata"] = tags
        author = tags.get("Artist") or tags.get("OwnerName")
        record["author"] = author.strip() if isinstance(author, str) and author.strip() else None
        json_write(cache / (record["file"] + ".json"), record)
        print(f"Validated real {record['file_type']} + {preview['width']}x{preview['height']} {preview['tag']}: {path.name}", flush=True)

    output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix=".raw-demo-", dir=output.parent) as temporary:
        staging = Path(temporary) / "raw"
        staging.mkdir()
        specs = fixture_specs(sources)
        for spec in specs:
            path = staging / spec["file"]
            if spec["file_type"] == "JPEG":
                # JPEG anchors use the actual embedded photograph, in memory.
                with Image.open(io.BytesIO(preview_data[spec["source_id"]])) as image:
                    image.convert("RGB").save(path, "JPEG", quality=90)
            else:
                shutil.copyfile(cache / spec["source_file"], path)
            try:
                write_simulated_metadata(command, path, spec)
                actual = metadata(command, path)
                if actual.get("FileType", "").upper() != spec["file_type"]:
                    raise RuntimeError("Metadata update changed the original file type")
                if spec["file_type"] != "JPEG" and actual.get("Model") != next(r["original_metadata"].get("Model") for r in records if r["id"] == spec["source_id"]):
                    raise RuntimeError("Camera model was lost during metadata update")
                if actual.get("DateTimeOriginal") != datetime.fromisoformat(spec["captured_at"]).strftime("%Y:%m:%d %H:%M:%S"):
                    raise RuntimeError("Capture time did not round-trip")
                if spec["gps"] is None:
                    if "GPSLatitude" in actual or "GPSLongitude" in actual:
                        raise RuntimeError("GPS was not removed from generated missing-GPS fixture")
                else:
                    if any(abs(float(actual.get(tag, 999)) - spec["gps"][axis]) > 1e-7 for tag, axis in (("GPSLatitude", "lat"), ("GPSLongitude", "lon"))):
                        raise RuntimeError("Synthetic GPS did not round-trip")
                if spec["file_type"] != "JPEG":
                    _, spec["embedded_preview"] = embedded_preview(command, path)
            except Exception as error:
                json_write(cache / "generation-error.json", {"file": spec["file"], "file_type": spec["file_type"], "writable": False, "error_type": type(error).__name__, "error": "Fixture generation failed; inspect the local console output."})
                raise RuntimeError(f"Cannot prepare writable {spec['file_type']} fixture {spec['file']}: {error}") from error
            spec["writable"] = True
            spec["expected"] = {"status": "existing-gps" if spec["gps"] else "candidate", "candidate": None if spec["gps"] else {"coordinates": spec["expected_coordinates"], "mode": "auto", "method": spec["expected_method"]}}
            spec["sha256"] = sha256_file(path)
            os.utime(path, ns=(FIXTURE_TIMESTAMP_NS, FIXTURE_TIMESTAMP_NS))
            stats = path.stat()
            spec["file_times"] = {"accessed_ns": stats.st_atime_ns, "modified_ns": stats.st_mtime_ns}
            print(f"Prepared {spec['file']}: {'green' if spec['gps'] else 'yellow'}", flush=True)
        manifest = {"signature": SIGNATURE, "warning": "Real complete camera RAWs; fixture GPS and capture dates are simulated, unrelated to the actual scene. Original files remain in raw-sources.", "repository_url": "https://raw.pixls.us/", "license": "CC0-1.0", "defaults": {"adjacent_gap_seconds": 60, "reference_window_seconds": 600, "session_gap_seconds": 3600}, "counts": {"photos": len(specs), "raw_formats": 9, "makers": 8, "green": 5, "yellow": 9, "red": 0, "sessions": 3}, "sources": records, "photos": specs}
        json_write(staging / "manifest.json", manifest)
        json_write(staging / "expected.json", manifest)
        (staging / "README.txt").write_text("PhotoPinFix genuine RAW demo\n\nAll GPS and capture times are simulated.\n9 missing-GPS RAWs: Sony ARW, Canon CR2+CR3, Leica DNG, Nikon NEF, Fujifilm RAF, Panasonic RW2, Olympus ORF, Pentax PEF.\nOne additional Sony RAW and four JPEG anchors already have GPS.\nDefault expected result: 3 sessions, 5 green, 9 yellow, 0 red.\nSession 1/3 use time interpolation; session 2 copies the next GPS.\nOriginal CC0 source URLs, SHA256, author metadata and expected coordinates: manifest.json.\nThis covers these camera samples, not every variant of each vendor's RAW format.\n", encoding="utf-8")
        if output.exists():
            backup = output.with_name(output.name + ".previous")
            if backup.exists():
                raise RuntimeError(f"Refusing to overwrite existing backup {backup}")
            output.rename(backup)
            try:
                staging.rename(output)
            except Exception:
                backup.rename(output)
                raise
            shutil.rmtree(backup)
        else:
            staging.rename(output)
    print(f"Created {output}: 14 photos, 9 genuine missing-GPS RAW formats / 8 makers; 3 sessions, 5 green, 9 yellow.", flush=True)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--exiftool", type=Path, default=PROJECT / "tools/exiftool/exiftool")
    parser.add_argument("--output", type=Path, default=PROJECT / "demo/raw")
    parser.add_argument("--reset", action="store_true", help="replace only a recognized generated RAW demo")
    parser.add_argument("--plan", action="store_true", help="show pinned sources and sizes without downloads or writes")
    args = parser.parse_args()
    sources = json.loads(CATALOG.read_text(encoding="utf-8"))["sources"]
    if args.plan:
        print(json.dumps({"approx_download_mib": round(sum(s["approx_mib"] for s in sources), 2), "sources": sources, "photos": fixture_specs(sources)}, ensure_ascii=False, indent=2))
        return 0
    try:
        build(args, sources)
        return 0
    except Exception as error:
        print(f"RAW demo generation failed: {error}", file=sys.stderr, flush=True)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
