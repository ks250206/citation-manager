#!/usr/bin/env python3
"""Download the current Stable "Chrome for Testing" for macOS arm64 and print its executable.

Branded Google Chrome (137+) ignores `--load-extension`, so the hosted Native Messaging
probe falls back to Chrome for Testing, Google's automation build of the same Chromium
version, which still honours it. Test-only: the download is never bundled or installed.

  python3 scripts/package/layout-smoke/chrome_for_testing.py DEST_DIR
"""
import json
from pathlib import Path
import subprocess
import sys
import urllib.request

INDEX = 'https://googlechromelabs.github.io/chrome-for-testing/last-known-good-versions-with-downloads.json'
PLATFORM = 'mac-arm64'


def download_url(index, platform=PLATFORM):
    stable = index['channels']['Stable']
    for entry in stable['downloads']['chrome']:
        if entry['platform'] == platform:
            return stable['version'], entry['url']
    raise ValueError(f'no {platform} build in the Stable channel')


def main():
    destination = Path(sys.argv[1]).resolve()
    destination.mkdir(parents=True, exist_ok=True)
    with urllib.request.urlopen(INDEX, timeout=60) as response:
        version, url = download_url(json.loads(response.read()))
    archive = destination / 'chrome.zip'
    with urllib.request.urlopen(url, timeout=300) as response:
        archive.write_bytes(response.read())
    subprocess.run(['ditto', '-x', '-k', str(archive), str(destination)], check=True)
    app = destination / f'chrome-{PLATFORM}/Google Chrome for Testing.app'
    subprocess.run(['xattr', '-dr', 'com.apple.quarantine', str(app)], check=False)
    print(f'Chrome for Testing {version}', file=sys.stderr)
    print(app / 'Contents/MacOS/Google Chrome for Testing')


if __name__ == '__main__':
    main()
