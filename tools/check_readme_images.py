#!/usr/bin/env python3
"""Validate README screenshots locally and through their public release URLs."""

from __future__ import annotations

import re
import subprocess
import sys
import urllib.request
from pathlib import Path
from urllib.parse import urlsplit


README = Path(__file__).resolve().parents[1] / "README.md"
ROOT = README.parent
MARKDOWN_IMAGE_RE = re.compile(r"!\[[^\]]*\]\(([^)\s]+)")
HTML_IMAGE_RE = re.compile(r"<img\b[^>]*\bsrc=[\"']([^\"']+)[\"']", re.IGNORECASE)
PNG_SIGNATURE = b"\x89PNG\r\n\x1a\n"
RELEASE_PREFIX = "https://github.com/simfor99/herdr-night-watch/releases/download/"
LOCAL_SOURCE_IMAGES = (
    ROOT / "docs/images/live-status-de-latest.png",
    ROOT / "docs/images/live-status-en-latest.png",
)


def read_image_references() -> list[str]:
    markdown = README.read_text(encoding="utf-8")
    return MARKDOWN_IMAGE_RE.findall(markdown) + HTML_IMAGE_RE.findall(markdown)


def check_local_sources() -> bool:
    passed = True
    for path in LOCAL_SOURCE_IMAGES:
        relative = path.relative_to(ROOT)
        if not path.is_file():
            print(f"FAIL: missing local source image: {relative}")
            passed = False
            continue
        if path.stat().st_mode & 0o777 != 0o644:
            print(f"FAIL: local source image must have mode 644: {relative}")
            passed = False
            continue
        try:
            subprocess.run(
                ["git", "ls-files", "--error-unmatch", "--", str(relative)],
                cwd=ROOT,
                check=True,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
            )
        except subprocess.CalledProcessError:
            print(f"FAIL: local source image is not tracked: {relative}")
            passed = False
            continue
        print(f"PASS: local source {relative}")
    return passed


def main() -> int:
    references = read_image_references()
    if not references:
        print("FAIL: README contains no image references")
        return 1

    failed = not check_local_sources()
    for url in references:
        parsed = urlsplit(url)
        if parsed.scheme not in {"http", "https"}:
            print(f"FAIL: README image must use a public release URL: {url}")
            failed = True
            continue
        if not url.startswith(RELEASE_PREFIX) or not parsed.path.lower().endswith(".png"):
            print(f"FAIL: README image is not a GitHub release PNG: {url}")
            failed = True
            continue
        request = urllib.request.Request(url, headers={"User-Agent": "herdr-night-watch-readme-check"})
        try:
            with urllib.request.urlopen(request, timeout=20) as response:
                payload = response.read(32)
                status = getattr(response, "status", response.getcode())
                content_type = response.headers.get_content_type()
        except Exception as exc:  # noqa: BLE001 - report the concrete URL failure
            print(f"FAIL: {url}: {exc}")
            failed = True
            continue

        valid_type = content_type in {"image/png", "application/octet-stream"}
        if status != 200 or (not valid_type and payload[:8] != PNG_SIGNATURE):
            print(f"FAIL: {url}: status={status}, content-type={content_type}")
            failed = True
            continue
        if payload[:8] != PNG_SIGNATURE:
            print(f"FAIL: {url}: response is not a PNG")
            failed = True
            continue
        print(f"PASS: {url}")

    return int(failed)


if __name__ == "__main__":
    sys.exit(main())
