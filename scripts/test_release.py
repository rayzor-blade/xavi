#!/usr/bin/env python3
"""Check the SDK's distribution contract and release completeness."""
import hashlib
import json
from pathlib import Path
import tempfile
import unittest
from zipfile import ZipFile

from package_release import ROOT, HOSTS, TARGETS, checksums, sdk, validate_tag


class ReleaseTest(unittest.TestCase):
    def test_sdk_is_reproducible_and_contains_relocatable_sources(self):
        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary)
            path = sdk(output, "nightly", allow_dirty=True)
            original = path.read_bytes()
            self.assertEqual(original, sdk(output, "nightly", allow_dirty=True).read_bytes())
            with ZipFile(path) as archive:
                manifest = json.loads(archive.read("xavi-sdk.json"))
                self.assertEqual(manifest["schema"], 1)
                self.assertEqual(manifest["targets"], TARGETS)
                self.assertEqual(set(archive.namelist()), set(manifest["files"]) | {"xavi-sdk.json"})
                for name, digest in manifest["files"].items():
                    self.assertEqual(hashlib.sha256(archive.read(name)).hexdigest(), digest)
                    self.assertFalse(set(Path(name).parts) & {".git", "target", "__pycache__"})
                for name in ["xavi/api/spec/media.idl", "xavi/api/media.api.rs",
                             "xavi/Cargo.lock", "xavi/crates/xavi-platform/src/player/apple.m",
                             "xavi/crates/xavi-backend/src/template/native.rs",
                             "x-idl/Cargo.toml", "x-idl/LICENSE", "x-idl/tests/fixtures/window.idl"]:
                    self.assertIn(name, manifest["files"])
                self.assertIn('path = "../x-idl"', archive.read("xavi/Cargo.toml").decode())
                self.assertEqual(manifest["x-idl"], json.loads((ROOT / "release-sources.json").read_text())["x-idl"])

    def test_release_requires_every_host_and_checksums_exact_bytes(self):
        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary)
            (output / "xavi-sdk.zip").write_bytes(b"sdk")
            with self.assertRaises(ValueError):
                checksums(output, complete=True)
            for host in HOSTS:
                (output / f"xavi-tools-{host}.zip").write_bytes(host.encode())
            lines = checksums(output, complete=True).read_text().splitlines()
            self.assertEqual(len(lines), len(HOSTS) + 1)
            for line in lines:
                digest, name = line.split("  ")
                self.assertEqual(digest, hashlib.sha256((output / name).read_bytes()).hexdigest())

    def test_release_tags(self):
        for tag in ["nightly", "v0.1.0", "v2026.10.07", "v1.2.3-rc.1"]:
            self.assertEqual(validate_tag(tag), tag)
        for tag in ["main", "v1.2", "v1.2.3/../../other", "nightly\nextra"]:
            with self.assertRaises(ValueError):
                validate_tag(tag)


if __name__ == "__main__":
    unittest.main(verbosity=2)
