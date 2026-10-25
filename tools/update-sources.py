#!/usr/bin/env python3
"""Regenerates sources.json, the release binaries the flake's versioned packages install
(`nix run 'github:OmarSiwy/Schemify#latest'`, `nix build '.#"1.0.0"'`), from the GitHub
releases. After VerA's (and mitchellh/zig-overlay's) sources.json:

    { "<version>": { "date": "YYYY-MM-DD",
                     "<nix system>": { "url": ..., "sha256": <hex> }, ... } }

The hashes are the release's own SHA256SUMS (written by publish.yaml), so an archive that
does not match what publish.yaml uploaded fails the Nix fetch. Drafts and prereleases are
left out: a version enters sources.json when its release is published.

Usage: tools/update-sources.py [OUT]   (default: sources.json beside tools/)
GITHUB_TOKEN, when set, lifts the API's anonymous rate limit.
"""

import json
import os
import re
import sys
import urllib.request

REPO = "OmarSiwy/Schemify"
# Nix system -> the platform publish.yaml names the archive after. aarch64-linux has no
# release archive: the flake builds it from source.
SYSTEMS = {
    "x86_64-linux": "linux-x86_64",
    "x86_64-darwin": "macos-x86_64",
    "aarch64-darwin": "macos-aarch64",
}


def get(url):
    req = urllib.request.Request(url, headers={"User-Agent": "schemify-update-sources"})
    if "api.github.com" in url and os.environ.get("GITHUB_TOKEN"):
        req.add_header("Authorization", "Bearer " + os.environ["GITHUB_TOKEN"])
    with urllib.request.urlopen(req) as r:
        return r.read().decode()


def main():
    here = os.path.dirname(os.path.abspath(__file__))
    out = sys.argv[1] if len(sys.argv) > 1 else os.path.join(here, "..", "sources.json")
    sources = {}
    for rel in json.loads(get(f"https://api.github.com/repos/{REPO}/releases?per_page=100")):
        tag = rel["tag_name"]
        if rel["draft"] or rel["prerelease"] or not re.fullmatch(r"v\d+\.\d+\.\d+", tag):
            continue
        assets = {a["name"]: a["browser_download_url"] for a in rel["assets"]}
        if "SHA256SUMS" not in assets:
            continue
        sums = {}
        for line in get(assets["SHA256SUMS"]).splitlines():
            if line.strip():
                digest, name = line.split()
                sums[name.lstrip("*")] = digest
        entry = {"date": rel["published_at"][:10]}
        for system, platform in SYSTEMS.items():
            name = f"schemify-{tag}-{platform}.tar.gz"
            if name in assets and name in sums:
                entry[system] = {"url": assets[name], "sha256": sums[name]}
        if len(entry) > 1:
            sources[tag[1:]] = entry
    if not sources:
        print("update-sources: no published release with SHA256SUMS yet", file=sys.stderr)
    with open(out, "w") as f:
        json.dump(sources, f, indent=2, sort_keys=True)
        f.write("\n")


if __name__ == "__main__":
    main()
