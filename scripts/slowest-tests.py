#!/usr/bin/env python3
"""Печатает самые долгие тесты прогона nextest по его junit.xml."""
import sys
import xml.etree.ElementTree as ET

path = sys.argv[1]
count = int(sys.argv[2]) if len(sys.argv) > 2 else 10
rows = sorted(
    (
        (float(case.get("time", 0)), case.get("classname", ""), case.get("name", ""))
        for case in ET.parse(path).iter("testcase")
    ),
    reverse=True,
)
print(f"slowest tests of {len(rows)}:")
for seconds, binary, name in rows[:count]:
    print(f"  {seconds:6.1f}s  {binary} {name}")
