#!/usr/bin/env python3
"""Prepend a new <item> to an existing appcast.xml's first <channel>.

Called by publish-metadata.yml (Phase 3d) when a GitHub Release is published.

Stdlib only — no PyPI deps.
"""

import argparse
import email.utils
import sys
import xml.etree.ElementTree as ET


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
    # description_html is HTML (rendered upstream from Markdown via pandoc).
    # ElementTree XML-escapes desc.text once on serialize, producing
    # &lt;h1&gt;… in the appcast. Sparkle XML-decodes once when parsing the
    # feed, then feeds the resulting HTML to its WebView. Do NOT escape
    # here — the manual escape() that used to live here caused
    # double-escaping (&amp;lt;h1&amp;gt;) and the dialog rendered as raw
    # markup. (v0.1.1 incident, 2026-05-04.)
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
    expected RSS+Sparkle shape.

    Idempotent: if an item with the same sparkle:shortVersionString already
    exists in the channel, the call is a no-op. publish-metadata.yml may run
    twice for the same release (workflow_dispatch retry, accidental re-publish);
    duplicates would otherwise show up to Sparkle as two updates with identical
    versions and confuse users."""
    tree = ET.parse(appcast_path)
    root = tree.getroot()
    if root.tag != "rss":
        raise ValueError(f"expected <rss> root, got <{root.tag}>")

    channel = root.find("channel")
    if channel is None:
        raise ValueError("<channel> not found in appcast.xml")

    new_short = item.findtext(f"{{{SPARKLE_NS}}}shortVersionString")
    for existing in channel.findall("item"):
        existing_short = existing.findtext(f"{{{SPARKLE_NS}}}shortVersionString")
        if existing_short == new_short:
            print(
                f"skip: appcast already has an item with sparkle:shortVersionString={new_short!r}"
            )
            return

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
    p.add_argument(
        "--version",
        required=True,
        help=(
            "Value for <sparkle:version>. MUST be the app's CFBundleVersion "
            "(monotonic build number, e.g. '241'), NOT the marketing string. "
            "Sparkle's SUStandardVersionComparator compares this against the "
            "installed CFBundleVersion; passing '0.1.1' against an installed "
            "build of 240 is parsed as [0,1,1] vs [240,0,0] and Sparkle then "
            "reports 'up to date' (v0.1.1 incident, 2026-05-04)."
        ),
    )
    p.add_argument(
        "--short-version",
        required=True,
        help="Value for <sparkle:shortVersionString>. The user-facing marketing label (e.g. '0.1.1').",
    )
    p.add_argument("--enclosure-url", required=True)
    p.add_argument("--ed-signature", required=True)
    p.add_argument("--length", required=True, type=int)
    p.add_argument(
        "--description-file",
        required=True,
        help="HTML file with release notes (typically rendered from Markdown via pandoc by the workflow)",
    )
    p.add_argument("--pub-date", default=None, help="RFC 2822 date (default: now)")
    args = p.parse_args()

    with open(args.description_file) as f:
        description_html = f.read().strip()

    pub_date = args.pub_date or email.utils.formatdate(usegmt=True)

    item = build_item_xml(
        version=args.version,
        short_version=args.short_version,
        enclosure_url=args.enclosure_url,
        ed_signature=args.ed_signature,
        length=args.length,
        description_html=description_html,
        pub_date=pub_date,
    )

    append_item_to_appcast(args.appcast, item)
    print(f"appended {args.short_version} to {args.appcast}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
