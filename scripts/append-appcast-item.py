#!/usr/bin/env python3
"""Prepend a new <item> to an existing appcast.xml's first <channel>.

Called by publish-metadata.yml (Phase 3d) when a GitHub Release is published.

Stdlib only — no PyPI deps.
"""

import argparse
import email.utils
import sys
import xml.etree.ElementTree as ET
from xml.sax.saxutils import escape


SPARKLE_NS = "http://www.andymatuschak.org/xml-namespaces/sparkle"

# Register the sparkle namespace so ElementTree doesn't rewrite sparkle:foo
# into ns0:foo on serialize.
ET.register_namespace("sparkle", SPARKLE_NS)


def build_item_xml(
    *,
    version: str,
    short_version: str,
    enclosure_url: str,
    ed_signature: str,
    length: int,
    description_html: str,
    pub_date: str,
) -> ET.Element:
    """Return a <item> Element with the Sparkle-convention children."""
    item = ET.Element("item")

    title = ET.SubElement(item, "title")
    title.text = f"Version {short_version}"

    sparkle_version = ET.SubElement(item, f"{{{SPARKLE_NS}}}version")
    sparkle_version.text = str(version)

    sparkle_short = ET.SubElement(item, f"{{{SPARKLE_NS}}}shortVersionString")
    sparkle_short.text = short_version

    desc = ET.SubElement(item, "description")
    # Wrap in CDATA by hex-escaping — but ElementTree serializes text as
    # escaped characters, which browsers and Sparkle both accept. Don't
    # try to emit raw CDATA sections (stdlib doesn't support it cleanly).
    desc.text = description_html

    pub = ET.SubElement(item, "pubDate")
    pub.text = pub_date

    enc = ET.SubElement(item, "enclosure")
    enc.set("url", enclosure_url)
    enc.set("length", str(length))
    enc.set("type", "application/octet-stream")
    enc.set(f"{{{SPARKLE_NS}}}edSignature", ed_signature)

    return item


def append_item_to_appcast(
    appcast_path: str,
    item: ET.Element,
) -> None:
    """Parse the existing appcast.xml, prepend `item` to its first <channel>,
    and write the result back. Raises if the file doesn't match the
    expected RSS+Sparkle shape."""
    tree = ET.parse(appcast_path)
    root = tree.getroot()
    if root.tag != "rss":
        raise ValueError(f"expected <rss> root, got <{root.tag}>")

    channel = root.find("channel")
    if channel is None:
        raise ValueError("<channel> not found in appcast.xml")

    # Find the first existing <item> (if any) — we want to insert before it,
    # so newest-first ordering is preserved. If none exist, append to channel.
    existing_item = channel.find("item")
    if existing_item is not None:
        idx = list(channel).index(existing_item)
        channel.insert(idx, item)
    else:
        channel.append(item)

    tree.write(appcast_path, xml_declaration=True, encoding="utf-8")


def main() -> int:
    p = argparse.ArgumentParser(description="Prepend an item to appcast.xml")
    p.add_argument("--appcast", required=True, help="path to appcast.xml to modify in place")
    p.add_argument("--version", required=True, help="Sparkle numeric version (often monotonic int or semver)")
    p.add_argument("--short-version", required=True, help="User-facing version string")
    p.add_argument("--enclosure-url", required=True)
    p.add_argument("--ed-signature", required=True)
    p.add_argument("--length", required=True, type=int)
    p.add_argument("--description-file", required=True, help="markdown/html file with release notes")
    p.add_argument("--pub-date", default=None, help="RFC 2822 date (default: now)")
    args = p.parse_args()

    with open(args.description_file) as f:
        description = f.read().strip()

    # Description field: pre-escape any XML-unsafe characters so that
    # browsers rendering the feed (and Sparkle itself) receive safe HTML.
    description_safe = escape(description)

    pub_date = args.pub_date or email.utils.formatdate(usegmt=True)

    item = build_item_xml(
        version=args.version,
        short_version=args.short_version,
        enclosure_url=args.enclosure_url,
        ed_signature=args.ed_signature,
        length=args.length,
        description_html=description_safe,
        pub_date=pub_date,
    )

    append_item_to_appcast(args.appcast, item)
    print(f"appended {args.short_version} to {args.appcast}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
