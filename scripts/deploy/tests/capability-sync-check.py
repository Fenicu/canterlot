#!/usr/bin/env python3
# Guard: the capability filters in the worker's advertised_capabilities and the
# conditional list in verify.sh must name exactly the same capabilities. Both
# files are read as text; no project code is imported or executed.
import re
import sys
from pathlib import Path


def fail(message):
    print(f"capability-sync: {message}", file=sys.stderr)
    sys.exit(1)


session_path, verify_path = (Path(arg) for arg in sys.argv[1:3])
session = session_path.read_text()
verify = verify_path.read_text()

function = re.search(r"pub fn advertised_capabilities\b.*?\n\}", session, re.S)
if not function:
    fail(f"advertised_capabilities not found in {session_path}")

names = set()
for line in function.group(0).splitlines():
    if ".filter(" not in line:
        continue
    for literal in re.findall(r'"([a-z0-9_]+)"', line):
        names.add(literal)
    for constant in re.findall(r"\bCAPABILITY_[A-Z0-9_]+\b", line):
        definition = re.search(
            rf'pub const {constant}:\s*&str\s*=\s*"([a-z0-9_]+)"', session
        )
        if not definition:
            fail(f"constant {constant} in advertised_capabilities has no known value")
        names.add(definition.group(1))

if not names:
    fail("no conditional capability filters found in advertised_capabilities")

declared = re.search(r'^CONDITIONAL_CAPABILITIES="([a-z0-9_ ]*)"$', verify, re.M)
if not declared:
    fail(f"CONDITIONAL_CAPABILITIES list not found in {verify_path}")
known = set(declared.group(1).split())

if names != known:
    fail(
        "conditional capabilities out of sync: "
        f"session.rs={sorted(names)}, verify.sh={sorted(known)}; "
        "update CONDITIONAL_CAPABILITIES and the drop rules in verify.sh"
    )

print(f"capability-sync: {', '.join(sorted(names))}")
