#!/usr/bin/env python3
"""Check Cargo dependency license metadata for publishable policy drift."""

from __future__ import annotations

import json
import subprocess
import sys


PERMISSIVE_OR_WEAK_COPYLEFT_ALLOWED = {
    "0BSD",
    "Apache-2.0",
    "BSD-2-Clause",
    "BSD-3-Clause",
    "BSL-1.0",
    "CC0-1.0",
    "ISC",
    "MIT",
    "MPL-2.0",
    "NCSA",
    "Unicode-3.0",
    "Unlicense",
    "Zlib",
}


def cargo_metadata() -> dict:
    command = [
        "cargo",
        "metadata",
        "--format-version",
        "1",
        "--all-features",
        "--locked",
    ]
    result = subprocess.run(command, check=True, capture_output=True, text=True)
    return json.loads(result.stdout)


def has_allowed_branch(expression: str) -> bool:
    return any(token in expression for token in PERMISSIVE_OR_WEAK_COPYLEFT_ALLOWED)


def main() -> int:
    metadata = cargo_metadata()
    failures: list[str] = []

    for package in sorted(metadata["packages"], key=lambda item: (item["name"], item["version"])):
        name = package["name"]
        version = package["version"]
        license_expr = package.get("license")
        license_file = package.get("license_file")

        if not license_expr and not license_file:
            failures.append(f"{name} {version}: missing license metadata")
            continue

        if not license_expr:
            continue

        if "AGPL" in license_expr:
            failures.append(f"{name} {version}: AGPL expression {license_expr!r}")
            continue

        if ("GPL" in license_expr or "LGPL" in license_expr) and not has_allowed_branch(license_expr):
            failures.append(f"{name} {version}: copyleft-only expression {license_expr!r}")

    if failures:
        print("Dependency license check failed:", file=sys.stderr)
        for failure in failures:
            print(f"- {failure}", file=sys.stderr)
        return 1

    print(f"Dependency license check passed for {len(metadata['packages'])} packages.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
