#!/usr/bin/env python3
"""Generic authoring-pipeline contract refusals.

These exercise generic JSON/publication admission and the surviving package
selection/profile surface. They start no client/server, require no deployed
plugin package, and claim no runtime acceptance.
"""
from __future__ import annotations

import tempfile
import subprocess
import sys
from pathlib import Path
import unittest

from content_pipeline import read_json, publish
import build_standard_pack


class AuthoringContractRefusals(unittest.TestCase):
    def test_duplicate_json_field_cannot_silently_replace_asset_digest(self):
        with tempfile.TemporaryDirectory(prefix="solaris-duplicate-json-") as temporary:
            path = Path(temporary) / "asset.json"
            path.write_text('{"sha256":"reviewed","sha256":"replacement"}')
            with self.assertRaises(ValueError):
                read_json(path)

    def test_immutable_publication_preserves_existing_artifacts(self):
        with tempfile.TemporaryDirectory(prefix="solaris-immutable-publication-") as temporary:
            root = Path(temporary)
            candidate = root / "candidate"
            candidate.mkdir()
            (candidate / "artifact").write_bytes(b"first admitted bytes")
            destination = root / "published"
            publish(candidate, destination)
            replacement = root / "replacement"
            replacement.mkdir()
            (replacement / "artifact").write_bytes(b"not admitted")
            with self.assertRaises(ValueError):
                publish(replacement, destination)
            self.assertEqual((destination / "artifact").read_bytes(), b"first admitted bytes")

    def test_optional_chisel_is_registered_but_not_in_default_selection(self):
        self.assertIn("chisel",
                      (package.package_id for package in build_standard_pack.PACKAGES))
        self.assertNotIn("chisel",
                         (package.package_id for package in build_standard_pack.STANDARD_PACKAGES))

    def test_retired_settlement_profile_is_refused_before_launch(self):
        result = subprocess.run(
            [sys.executable, str(Path(__file__).with_name("run_integration.py")),
             "settlement-cargo"],
            capture_output=True, text=True, check=False,
        )
        self.assertEqual(result.returncode, 2)
        self.assertIn("invalid choice", result.stderr)


if __name__ == "__main__":
    unittest.main()
