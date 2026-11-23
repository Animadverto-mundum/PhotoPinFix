#!/usr/bin/env python3
"""Create PhotoPinFix's clearly labelled, simulated-metadata photo dataset.

No user photos are read or changed. Downloads are public sample photographs.
Existing demo photos are left alone unless --reset is explicitly requested.
"""

from __future__ import annotations

import argparse
from concurrent.futures import ThreadPoolExecutor
from dataclasses import dataclass
from datetime import datetime, timedelta, timezone
import hashlib
import json
import math
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import time
from urllib.request import Request, urlopen


PROJECT = Path(__file__).resolve().parents[1]
PREVIEW_DEPENDENCIES = PROJECT / "tools/preview-deps"
if PREVIEW_DEPENDENCIES.is_dir():
    sys.path.insert(0, str(PREVIEW_DEPENDENCIES))
SIGNATURE = "PhotoPinFix simulated GPS test photos v1"
FILE_TIMESTAMP = datetime(2020, 1, 2, 3, 4, 5, tzinfo=timezone.utc)
SOURCE_IDS = (10, 11, 12, 16, 17, 18)
MAX_DOWNLOAD_BYTES = 12 * 1024 * 1024


@dataclass(frozen=True)
class Spec:
    label: str
    case: str
    session: int
    captured_at: datetime | None
    gps: tuple[float, float] | None = None
    expected_coordinates: tuple[float, float] | None = None
    expected_method: str | None = None
    provenance: tuple[str, str] | None = None


def build_specs() -> list[Spec]:
    """Explicit scenario expectations, independent of the application's engine."""
    photos: list[Spec] = []

    def add(label, case, session, base, seconds, gps=None, candidate=None, method=None, provenance=None):
        captured_at = None if base is None else datetime.fromisoformat(base) + timedelta(seconds=seconds)
        photos.append(Spec(label, case, session, captured_at, gps, candidate, method, provenance))

    # Session 1: the user-approved 15-minute example, with a photo every 30 s.
    a, b = (31.2304, 121.4737), (31.2394, 121.4917)
    for step in range(31):
        seconds = step * 30
        if step in (0, 30):
            add(f"chain_{seconds:04d}s_anchor", "15-minute-chain", 1, "2026-03-01T09:00:00", seconds, gps=a if step == 0 else b)
        elif seconds < 300:
            add(f"chain_{seconds:04d}s_copy_previous", "15-minute-chain", 1, "2026-03-01T09:00:00", seconds, candidate=a, method="copy-previous")
        elif seconds <= 600:
            weight = seconds / 900
            value = tuple(round(left + (right - left) * weight, 9) for left, right in zip(a, b))
            add(f"chain_{seconds:04d}s_interpolate", "15-minute-chain", 1, "2026-03-01T09:00:00", seconds, candidate=value, method="time-interpolation")
        else:
            add(f"chain_{seconds:04d}s_copy_next", "15-minute-chain", 1, "2026-03-01T09:00:00", seconds, candidate=b, method="copy-next")

    # Session 2: exact 60 s and greater-than-60 s gaps stop propagation.
    # The later green photos replace the earlier reference as hard boundaries.
    c, d, e = (35.6800, 139.7600), (35.6810, 139.7610), (35.6830, 139.7650)
    rows = [
        (0, "anchor_C", c, None, None),
        (59, "gap_59s_yellow", None, c, "copy-previous"),
        (119, "gap_exact_60s_red", None, None, None),
        (180, "gap_61s_red", None, None, None),
        (209, "short_step_after_break_red", None, None, None),
        (400, "anchor_D", d, None, None),
        (430, "between_D_E_yellow", None, (35.6820, 139.7630), "time-interpolation"),
        (460, "anchor_E_boundary", e, None, None),
        (490, "copy_E_not_D_yellow", None, e, "copy-previous"),
        (550, "second_exact_60s_red", None, None, None),
        (579, "after_second_break_red", None, None, None),
    ]
    for seconds, label, gps, candidate, method in rows:
        add(label, "strict-adjacent-gap-and-green-boundary", 2, "2026-03-01T11:00:00", seconds, gps, candidate, method)

    # Session 3: continuous 50 s steps preserve the original anchor's age.
    # 600 s is accepted, 601 s and later remain red despite their short steps.
    f = (48.8566, 2.3522)
    for seconds in range(0, 601, 50):
        add(f"one_side_{seconds:04d}s", "reference-window-600-601", 3, "2026-03-01T13:00:00", seconds,
            gps=f if seconds == 0 else None, candidate=None if seconds == 0 else f,
            method=None if seconds == 0 else "copy-previous")
    for seconds in (601, 630, 659):
        add(f"one_side_{seconds:04d}s_red", "reference-window-600-601", 3, "2026-03-01T13:00:00", seconds)

    # Exactly one hour remains in the same session; one hour plus one second splits.
    add("session_boundary_anchor", "session-exact-one-hour", 4, "2026-03-01T15:00:00", 0, gps=f)
    add("exactly_one_hour_same_session_red", "session-exact-one-hour", 4, "2026-03-01T15:00:00", 3600)
    add("one_hour_plus_one_second_new_session_red", "session-over-one-hour", 5, "2026-03-01T15:00:00", 7201)

    # Equal capture times must not divide by zero or invent a midpoint.
    add("same_time_00_anchor_A", "conflicting-same-time", 6, "2026-03-02T10:00:00", 0, gps=(40.0, -74.0))
    add("same_time_01_missing_red", "conflicting-same-time", 6, "2026-03-02T10:00:00", 0)
    add("same_time_02_anchor_B", "conflicting-same-time", 6, "2026-03-02T10:00:00", 0, gps=(41.0, -73.0))

    # Short longitude arc across the date line, rather than around Greenwich.
    add("dateline_anchor_east", "international-date-line", 7, "2026-03-02T11:30:00", 0, gps=(35.0, 179.9))
    add("dateline_midpoint_yellow", "international-date-line", 7, "2026-03-02T11:30:00", 30, candidate=(35.0, -180.0), method="time-interpolation")
    add("dateline_anchor_west", "international-date-line", 7, "2026-03-02T11:30:00", 60, gps=(35.0, -179.9))

    # Missing beginning, using a later photo that already carries our provenance.
    location = (35.6812, 139.7671)
    add("head_missing_00_yellow", "one-sided-future-and-provenance", 8, "2026-03-02T13:00:00", 0, candidate=location, method="copy-next")
    add("head_missing_30_yellow", "one-sided-future-and-provenance", 8, "2026-03-02T13:00:00", 30, candidate=location, method="copy-next")
    add("head_anchor_already_auto_green", "one-sided-future-and-provenance", 8, "2026-03-02T13:00:00", 60, gps=location, provenance=("auto", "time-interpolation"))

    # Missing capture times go to the final unknown-time session. File times are
    # deliberately unrelated to capture times and must not be substituted.
    add("unknown_time_no_gps_1_red", "unknown-capture-time", 9, None, 0)
    add("unknown_time_no_gps_2_red", "unknown-capture-time", 9, None, 0)
    add("unknown_time_existing_gps_green", "unknown-capture-time", 9, None, 0, gps=(51.5007, -0.1246))
    return photos


def download_source(source_id: int, directory: Path) -> dict:
    path = directory / f"picsum-{source_id}.jpg"
    record_path = path.with_suffix(".json")
    if path.is_file() and record_path.is_file():
        record = json.loads(record_path.read_text(encoding="utf-8"))
        if hashlib.sha256(path.read_bytes()).hexdigest() == record.get("sha256"):
            return record
    url = f"https://picsum.photos/id/{source_id}/960/640"
    last_error: Exception | None = None
    for attempt in range(3):
        try:
            request = Request(url, headers={"User-Agent": "PhotoPinFix-demo-generator/0.1"})
            with urlopen(request, timeout=35) as response:
                content = response.read(MAX_DOWNLOAD_BYTES + 1)
                final_url = response.geturl()
            if len(content) > MAX_DOWNLOAD_BYTES:
                raise ValueError("source image exceeded the download size limit")
            from PIL import Image
            import io
            with Image.open(io.BytesIO(content)) as image:
                if image.format != "JPEG":
                    raise ValueError(f"expected a real JPEG, received {image.format}")
                image.verify()
            record = {
                "id": source_id,
                "source_url": url,
                "resolved_url": final_url,
                "info_url": f"https://picsum.photos/id/{source_id}/info",
                "local_file": f"sources/{path.name}",
                "sha256": hashlib.sha256(content).hexdigest(),
                "note": "Public sample photograph; all fixture GPS and capture times are simulated and unrelated to its real subject.",
            }
            temporary = path.with_suffix(".download")
            temporary.write_bytes(content)
            temporary.replace(path)
            record_path.write_text(json.dumps(record, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
            print(f"Downloaded source {source_id}: {len(content)} bytes", flush=True)
            return record
        except Exception as error:
            last_error = error
            if attempt < 2:
                time.sleep(attempt + 1)
    raise RuntimeError(f"Cannot download {url}: {last_error}")


def exiftool_command(program: Path) -> list[str]:
    # The checked-out Perl executable is portable on Linux/macOS; Windows may use
    # --exiftool pointing at the official executable distribution instead.
    if program.suffix.lower() == ".exe":
        return [str(program)]
    perl = shutil.which("perl")
    if perl is None:
        raise RuntimeError("Perl was not found; pass --exiftool with the official ExifTool executable")
    return [perl, str(program)]


def metadata_arguments(spec: Spec) -> list[str]:
    arguments = [
        "-overwrite_original",
        "-all=",
        "-Artist=PhotoPinFix demonstration fixture",
        "-ImageDescription=Downloaded sample photo. GPS and capture time are simulated, unrelated to the actual scene.",
    ]
    if spec.captured_at is not None:
        captured = spec.captured_at.strftime("%Y:%m:%d %H:%M:%S")
        arguments += [f"-DateTimeOriginal={captured}", f"-CreateDate={captured}"]
    if spec.gps is not None:
        latitude, longitude = spec.gps
        arguments += [
            f"-GPSLatitude={abs(latitude):.9f}",
            f"-GPSLatitudeRef={'S' if latitude < 0 else 'N'}",
            f"-GPSLongitude={abs(longitude):.9f}",
            f"-GPSLongitudeRef={'W' if longitude < 0 else 'E'}",
            "-GPSMapDatum=WGS-84",
        ]
    if spec.provenance is not None:
        arguments += [
            f"-XMP-PhotoPinFix:Mode={spec.provenance[0]}",
            f"-XMP-PhotoPinFix:Method={spec.provenance[1]}",
        ]
    return arguments


def coordinates(value: tuple[float, float] | None) -> dict | None:
    return None if value is None else {"lat": value[0], "lon": value[1]}


def record_for(spec: Spec, filename: str, source_id: int) -> dict:
    candidate = None
    if spec.expected_coordinates is not None:
        candidate = {
            "coordinates": coordinates(spec.expected_coordinates),
            "mode": "auto",
            "method": spec.expected_method,
        }
    status = "green" if spec.gps is not None else "yellow" if candidate is not None else "red"
    return {
        "file": filename,
        "case": spec.case,
        "session": spec.session,
        "source_id": source_id,
        "captured_at": None if spec.captured_at is None else spec.captured_at.isoformat(),
        "gps": coordinates(spec.gps),
        "provenance": None if spec.provenance is None else {"mode": spec.provenance[0], "method": spec.provenance[1]},
        "expected": {"status": status, "candidate": candidate},
    }


def verify_metadata(command: list[str], config: Path, photo_directory: Path, records: list[dict]) -> None:
    arguments = command + [
        "-config", str(config), "-json", "-n", "-G1", "-s",
        "-DateTimeOriginal", "-GPSLatitude", "-GPSLongitude", "-GPSLatitudeRef", "-GPSLongitudeRef",
        "-XMP-PhotoPinFix:Mode", "-XMP-PhotoPinFix:Method",
        str(photo_directory),
    ]
    result = subprocess.run(arguments, check=True, capture_output=True, text=True, timeout=120)
    actual = {Path(entry["SourceFile"]).name: entry for entry in json.loads(result.stdout)}
    if len(actual) != len(records):
        raise RuntimeError(f"ExifTool read {len(actual)} photos; expected {len(records)}")
    for record in records:
        entry = actual[record["file"]]
        captured = next((v for key, v in entry.items() if key.endswith(":DateTimeOriginal")), None)
        expected_time = record["captured_at"]
        expected_time = None if expected_time is None else datetime.fromisoformat(expected_time).strftime("%Y:%m:%d %H:%M:%S")
        if captured != expected_time:
            raise RuntimeError(f"Capture time did not round-trip for {record['file']}: {captured!r}")
        lat, lon = entry.get("GPS:GPSLatitude"), entry.get("GPS:GPSLongitude")
        if lat is not None and entry.get("GPS:GPSLatitudeRef") == "S":
            lat = -abs(float(lat))
        if lon is not None and entry.get("GPS:GPSLongitudeRef") == "W":
            lon = -abs(float(lon))
        expected_gps = record["gps"]
        if expected_gps is None:
            if lat is not None or lon is not None:
                raise RuntimeError(f"Unexpected original GPS remains in {record['file']}")
        elif lat is None or lon is None or not math.isclose(float(lat), expected_gps["lat"], abs_tol=1e-7) or not math.isclose(float(lon), expected_gps["lon"], abs_tol=1e-7):
            raise RuntimeError(f"GPS did not round-trip for {record['file']}: {lat}, {lon}")
        expected_provenance = record["provenance"]
        if expected_provenance is not None:
            if entry.get("XMP-PhotoPinFix:Mode") != expected_provenance["mode"] or entry.get("XMP-PhotoPinFix:Method") != expected_provenance["method"]:
                raise RuntimeError(f"PhotoPinFix provenance did not round-trip for {record['file']}")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, default=PROJECT / "demo", help="Dataset root; photographs go into its photos/ directory")
    parser.add_argument("--exiftool", type=Path, default=PROJECT / "tools/exiftool/exiftool")
    parser.add_argument("--reset", action="store_true", help="Explicitly replace previously generated demo photos, discarding demo edits")
    options = parser.parse_args()
    root = options.output.resolve()
    photo_directory = root / "photos"
    marker = root / ".photopinfix-demo.json"
    if photo_directory.exists() and any(photo_directory.iterdir()):
        if not marker.is_file() or json.loads(marker.read_text(encoding="utf-8")).get("signature") != SIGNATURE:
            raise RuntimeError(f"Refusing to replace an unrecognized photo directory: {photo_directory}")
        if not options.reset:
            print(f"Existing demo left unchanged: {photo_directory}\nUse --reset only when you want to discard edits to the demo.")
            return 0
    from PIL import Image
    from pillow_heif import register_heif_opener
    register_heif_opener()
    command = exiftool_command(options.exiftool.resolve())
    config = PROJECT / "assets/photopinfix-exiftool.config"
    if not options.exiftool.is_file() or not config.is_file():
        raise RuntimeError("Local ExifTool or PhotoPinFix's metadata configuration is missing")
    root.mkdir(parents=True, exist_ok=True)
    sources_directory = root / "sources"
    sources_directory.mkdir(exist_ok=True)
    with ThreadPoolExecutor(max_workers=3) as executor:
        sources = list(executor.map(lambda source_id: download_source(source_id, sources_directory), SOURCE_IDS))
    specs = build_specs()
    records: list[dict] = []
    with tempfile.TemporaryDirectory(prefix=".photopinfix-demo-", dir=root) as temporary:
        staging = Path(temporary)
        staged_photos = staging / "photos"
        staged_photos.mkdir()
        for index, spec in enumerate(specs):
            source_id = SOURCE_IDS[index % len(SOURCE_IDS)]
            source = sources_directory / f"picsum-{source_id}.jpg"
            extension = ".heic" if index % 17 == 0 else ".png" if index % 11 == 0 else ".tif" if index % 13 == 0 else ".jpg"
            filename = f"{index + 1:03d}_S{spec.session:02d}_{spec.label}{extension}"
            output = staged_photos / filename
            if extension == ".jpg":
                shutil.copyfile(source, output)
            else:
                with Image.open(source) as image:
                    rgb = image.convert("RGB")
                    if extension == ".png":
                        rgb.save(output, format="PNG")
                    elif extension == ".heic":
                        # A real HEIF container encoded by libheif, not a renamed JPEG.
                        rgb.save(output, format="HEIF", quality=85)
                    else:
                        rgb.save(output, format="TIFF", compression="tiff_deflate")
            arguments = command + ["-config", str(config)] + metadata_arguments(spec) + [str(output)]
            result = subprocess.run(arguments, capture_output=True, text=True, timeout=40)
            if result.returncode != 0:
                raise RuntimeError(f"Could not set metadata on {filename}: {result.stderr.strip()}")
            record = record_for(spec, filename, source_id)
            records.append(record)
            print(f"[{index + 1:02d}/{len(specs)}] {record['expected']['status']:6s} {filename}", flush=True)

        verify_metadata(command, config, staged_photos, records)
        timestamp_ns = int(FILE_TIMESTAMP.timestamp()) * 1_000_000_000
        for record in records:
            path = staged_photos / record["file"]
            os.utime(path, ns=(timestamp_ns, timestamp_ns))
            stat = path.stat()
            record["file_times"] = {
                "atime_ns": stat.st_atime_ns,
                "mtime_ns": stat.st_mtime_ns,
                "note": "Unix ctime is inode-change time, not creation time, and cannot be assigned through os.utime.",
            }
        sessions = []
        for session_id in sorted({record["session"] for record in records}):
            group = [record for record in records if record["session"] == session_id]
            sessions.append({
                "id": session_id,
                "count": len(group),
                "start": group[0]["captured_at"],
                "end": group[-1]["captured_at"],
                "cases": sorted({record["case"] for record in group}),
            })
        manifest = {
            "signature": SIGNATURE,
            "simulated_metadata": True,
            "warning": "GPS and capture times are invented for tests and do not describe the downloaded photographs' actual locations or dates.",
            "defaults": {"adjacent_gap_seconds": 60, "reference_window_seconds": 600, "session_gap_seconds": 3600},
            "fixed_file_timestamp_utc": FILE_TIMESTAMP.isoformat(),
            "photo_directory": "photos",
            "total": len(records),
            "counts": {status: sum(record["expected"]["status"] == status for record in records) for status in ("green", "yellow", "red")},
            "sources": sources,
            "sessions": sessions,
            "photos": records,
        }
        if photo_directory.exists():
            shutil.rmtree(photo_directory)
        staged_photos.replace(photo_directory)
        for filename, content in [("expected.json", manifest), (".photopinfix-demo.json", {"signature": SIGNATURE})]:
            pending = staging / filename
            pending.write_text(json.dumps(content, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
            pending.replace(root / filename)
    print(f"Created and read-back verified {len(records)} photos in {photo_directory}", flush=True)
    print(f"Initial expected states: {manifest['counts']}; {len(sessions)} sessions including unknown capture times.")
    print(f"Run: cargo run -- {photo_directory}")
    print(f"Expectations and sources: {root / 'expected.json'}")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (RuntimeError, OSError, subprocess.SubprocessError, ValueError) as error:
        print(f"create_demo: {error}", file=sys.stderr)
        raise SystemExit(1)
