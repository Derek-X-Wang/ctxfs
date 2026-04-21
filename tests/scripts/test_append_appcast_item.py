"""Unit tests for scripts/append-appcast-item.py."""

import importlib.util
import pathlib
import tempfile
import unittest
import xml.etree.ElementTree as ET


def _load():
    repo_root = pathlib.Path(__file__).resolve().parents[2]
    spec = importlib.util.spec_from_file_location(
        "append_appcast", repo_root / "scripts" / "append-appcast-item.py"
    )
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


mod = _load()

SEED_APPCAST = """<?xml version='1.0' standalone='yes'?>
<rss version="2.0" xmlns:sparkle="http://www.andymatuschak.org/xml-namespaces/sparkle">
  <channel>
    <title>ContextFS Updates</title>
    <link>https://example.invalid/appcast.xml</link>
    <description>Updates</description>
    <language>en</language>
  </channel>
</rss>
"""


class BuildItemTests(unittest.TestCase):
    def test_item_has_sparkle_version_and_short_version(self):
        item = mod.build_item_xml(
            version="1",
            short_version="0.1.0",
            enclosure_url="https://example.invalid/app.zip",
            ed_signature="AA==",
            length=1,
            description_html="<p>Notes</p>",
            pub_date="Mon, 20 Apr 2026 00:00:00 GMT",
        )
        sv = item.find(f"{{{mod.SPARKLE_NS}}}version")
        ssv = item.find(f"{{{mod.SPARKLE_NS}}}shortVersionString")
        self.assertEqual(sv.text, "1")
        self.assertEqual(ssv.text, "0.1.0")

    def test_enclosure_attrs(self):
        item = mod.build_item_xml(
            version="1",
            short_version="0.1.0",
            enclosure_url="https://example.invalid/app.zip",
            ed_signature="SIG==",
            length=12345,
            description_html="notes",
            pub_date="Mon, 20 Apr 2026 00:00:00 GMT",
        )
        enc = item.find("enclosure")
        self.assertEqual(enc.get("url"), "https://example.invalid/app.zip")
        self.assertEqual(enc.get("length"), "12345")
        self.assertEqual(enc.get("type"), "application/octet-stream")
        self.assertEqual(
            enc.get(f"{{{mod.SPARKLE_NS}}}edSignature"), "SIG=="
        )


class AppendTests(unittest.TestCase):
    def _seed(self) -> str:
        tmp = tempfile.NamedTemporaryFile(
            mode="w", suffix=".xml", delete=False, encoding="utf-8"
        )
        tmp.write(SEED_APPCAST)
        tmp.close()
        return tmp.name

    def _item(self) -> ET.Element:
        return mod.build_item_xml(
            version="1",
            short_version="0.1.0",
            enclosure_url="https://example.invalid/app.zip",
            ed_signature="AA==",
            length=1,
            description_html="notes",
            pub_date="Mon, 20 Apr 2026 00:00:00 GMT",
        )

    def test_appends_to_empty_channel(self):
        path = self._seed()
        mod.append_item_to_appcast(path, self._item())
        tree = ET.parse(path)
        items = tree.getroot().findall("./channel/item")
        self.assertEqual(len(items), 1)

    def test_prepends_when_item_exists(self):
        path = self._seed()
        mod.append_item_to_appcast(path, self._item())  # first (for v0.1.0)
        # Second call with a different short-version goes at the top (newest first)
        second = mod.build_item_xml(
            version="2",
            short_version="0.2.0",
            enclosure_url="https://example.invalid/v2.zip",
            ed_signature="BB==",
            length=2,
            description_html="v2 notes",
            pub_date="Tue, 21 Apr 2026 00:00:00 GMT",
        )
        mod.append_item_to_appcast(path, second)

        tree = ET.parse(path)
        items = tree.getroot().findall("./channel/item")
        self.assertEqual(len(items), 2)
        first_title = items[0].find("title").text
        self.assertEqual(first_title, "Version 0.2.0")
        second_title = items[1].find("title").text
        self.assertEqual(second_title, "Version 0.1.0")

    def test_rejects_non_rss_root(self):
        tmp = tempfile.NamedTemporaryFile(
            mode="w", suffix=".xml", delete=False, encoding="utf-8"
        )
        tmp.write("<?xml version='1.0'?><nothing/>\n")
        tmp.close()
        with self.assertRaises(ValueError):
            mod.append_item_to_appcast(tmp.name, self._item())


if __name__ == "__main__":
    unittest.main()
