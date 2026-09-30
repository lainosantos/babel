"""Regression checks for generated documentation and project-site URLs."""
import tempfile
from pathlib import Path
import unittest

from build_site import ROOT, build, validate_links


class SiteTests(unittest.TestCase):
    def test_site_links_work_under_a_project_subpath(self):
        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary) / "babel"
            build(output)
            validate_links(output)
            self.assertTrue((output / "docs/index.html").is_file())
            content = (output / "docs/configuration.html").read_text(encoding="utf-8")
            self.assertIn('href="../assets/site.css"', content)
            self.assertIn('<html lang="en">', content)
            self.assertNotIn('href="/assets/', content)
            self.assertIn('<table>', content)
            self.assertIn('<pre><code', content)

    def test_build_does_not_replace_unrelated_files(self):
        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary)
            important = output / "keep.txt"
            important.write_text("Keep this file", encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "non-empty directory"):
                build(output)
            self.assertEqual(important.read_text(encoding="utf-8"), "Keep this file")
        with self.assertRaisesRegex(ValueError, "dedicated build directory"):
            build(ROOT)

    def test_link_validator_rejects_missing_fragments_and_assets(self):
        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary)
            (output / "index.html").write_text('<a href="guide.html#missing">Guide</a><img src="missing.svg">', encoding="utf-8")
            (output / "guide.html").write_text('<h1 id="present">Guide</h1>', encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "missing anchor guide.html#missing"):
                validate_links(output)


if __name__ == "__main__":
    unittest.main()
