#!/usr/bin/env python3
"""Reads `cargo metadata` JSON on stdin and prints the license texts of every crates.io
dependency compiled into the binary (for licenses/rust-crates.txt in the release bundle)."""
import glob
import json
import os
import sys

d = json.load(sys.stdin)
used = {n["id"] for n in d["resolve"]["nodes"]}
out = ["Third-party Rust crates compiled into the applejack binary.", ""]
for p in sorted(d["packages"], key=lambda p: p["name"]):
    if p["id"] not in used or p["source"] is None:
        continue
    out += ["=" * 78, f'{p["name"]} {p["version"]}  ({p["license"]})  {p.get("repository") or ""}', "=" * 78]
    root = os.path.dirname(p["manifest_path"])
    files = sorted(f for pat in ("LICEN[CS]E*", "COPYING*", "NOTICE*") for f in glob.glob(os.path.join(root, pat)))
    for f in files:
        out += [f"--- {os.path.basename(f)}", open(f, errors="replace").read().rstrip()]
    if not files:
        out.append("(the crate ships no license file; see the license expression above)")
    out.append("")
print("\n".join(out))
