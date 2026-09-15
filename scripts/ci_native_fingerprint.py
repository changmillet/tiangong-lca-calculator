#!/usr/bin/env python3
"""Fail-closed native identity for the Ubuntu dependency cache; stdout is hash only."""
import argparse
import hashlib
import json
import os
import re
import subprocess
import sys

REQUIRED_PACKAGES = {"libsuitesparse-dev", "libopenblas-dev", "liblapack-dev", "pkg-config"}
PACKAGE_QUERY = ["dpkg-query", "-W", "-f=${Package}\t${Version}\t${Architecture}\t${db:Status-Status}\n"]
PACKAGE_STATES = {"not-installed", "config-files", "half-installed", "unpacked", "half-configured", "triggers-awaited", "triggers-pending", "installed"}


class FingerprintError(ValueError):
    """Metadata is insufficient to admit a cache lookup."""


def command_output(argv):
    try:
        result = subprocess.run(argv, capture_output=True, text=True, timeout=30,
                                env={**os.environ, "LC_ALL": "C"})
    except (OSError, subprocess.SubprocessError, UnicodeError):
        raise FingerprintError(f"native metadata command failed: {argv[0]}") from None
    if result.returncode != 0 or not result.stdout.strip():
        raise FingerprintError(f"native metadata command failed or empty: {argv[0]}")
    if "\x00" in result.stdout:
        raise FingerprintError(f"invalid native metadata: {argv[0]}")
    return result.stdout.strip()


def packages(output):
    installed = []
    seen = set()
    for line in output.splitlines():
        fields = line.split("\t")
        if len(fields) != 4:
            raise FingerprintError("malformed package inventory")
        name, version, arch, status = fields
        if (not re.fullmatch(r"[a-z0-9][a-z0-9+.-]+", name)
                or not re.fullmatch(r"[a-z0-9][a-z0-9-]*", arch)
                or status not in PACKAGE_STATES
                or (name, arch) in seen):
            raise FingerprintError("invalid or duplicate package inventory")
        seen.add((name, arch))
        if status == "installed":
            if not re.fullmatch(r"[0-9][A-Za-z0-9.+:~\-]*", version):
                raise FingerprintError("invalid installed package version")
            installed.append([name, version, arch])
    if not REQUIRED_PACKAGES.issubset({item[0] for item in installed}):
        raise FingerprintError("required native packages are not installed")
    return sorted(installed)


def alternatives(output):
    selections = []
    seen = set()
    for line in output.splitlines():
        fields = line.split(None, 2)
        if (len(fields) != 3 or fields[1] not in {"auto", "manual"}
                or not fields[2].startswith("/") or fields[0] in seen):
            raise FingerprintError("invalid or duplicate alternatives inventory")
        seen.add(fields[0])
        selections.append(fields)
    if not selections:
        raise FingerprintError("empty alternatives inventory")
    return sorted(selections)


def fingerprint(environment):
    image = {}
    for name in ("ImageOS", "ImageVersion"):
        value = environment.get(name, "")
        if not isinstance(value, str) or not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._-]*", value):
            raise FingerprintError(f"missing or invalid runner identity: {name}")
        image[name] = value
    package_inventory = packages(command_output(PACKAGE_QUERY))
    selected_alternatives = alternatives(command_output(["update-alternatives", "--get-selections"]))
    compiler = command_output(["cc", "--version"])
    if not re.search(r"[0-9]+\.[0-9]+", compiler.splitlines()[0]):
        raise FingerprintError("invalid compiler version")
    pkg_config = command_output(["pkg-config", "--version"])
    if not re.fullmatch(r"[0-9]+(?:\.[0-9]+)+(?:[A-Za-z0-9.+~_-]*)", pkg_config):
        raise FingerprintError("invalid pkg-config version")
    evidence = {"schema": 1, "image": image, "packages": package_inventory,
                "alternatives": selected_alternatives, "cc_version": compiler,
                "pkg_config_version": pkg_config}
    canonical = json.dumps(evidence, sort_keys=True, separators=(",", ":"), ensure_ascii=True)
    return hashlib.sha256(canonical.encode("ascii")).hexdigest()


def main():
    argparse.ArgumentParser(description=__doc__).parse_args()
    try:
        digest = fingerprint(os.environ)
    except FingerprintError as error:
        print(f"native cache identity rejected: {error}", file=sys.stderr)
        return 1
    print(f"native_hash={digest}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
