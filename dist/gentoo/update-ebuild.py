#!/usr/bin/env python3
"""Fill CRATES and the dependent-crate LICENSE+= of a versioned ebuild from
Cargo.lock (what pycargoebuild does, without downloading anything: crate
licenses come from the local cargo registry, ~/.cargo/registry/src).

    python3 dist/gentoo/update-ebuild.py dist/gentoo/app-portage/portagehub/portagehub-0.4.0.ebuild

Run `cargo fetch` first so every crate is in the local registry.
"""

import glob
import os
import re
import sys
import tomllib

# SPDX identifier -> Gentoo license name
SPDX = {
    "0BSD": "0BSD",
    "Apache-2.0": "Apache-2.0",
    "Apache-2.0 WITH LLVM-exception": "Apache-2.0-with-LLVM-exceptions",
    "BSD-2-Clause": "BSD-2",
    "BSD-3-Clause": "BSD",
    "BSL-1.0": "Boost-1.0",
    "CC0-1.0": "CC0-1.0",
    "GPL-2.0-only": "GPL-2",
    "ISC": "ISC",
    "LGPL-2.1-or-later": "LGPL-2.1+",
    "MIT": "MIT",
    "MPL-2.0": "MPL-2.0",
    "OFL-1.1": "OFL-1.1",
    "Ubuntu-font-1.0": "UbuntuFontLicense-1.0",
    "Unicode-3.0": "Unicode-3.0",
    "Unicode-DFS-2016": "Unicode-DFS-2016",
    "Unlicense": "Unlicense",
    "Zlib": "ZLIB",
}


def alternatives(expr):
    """An SPDX expression as a list of alternatives, each a set of licenses
    that are all required ("A OR (B AND C)" -> [{A}, {B, C}])."""
    expr = expr.replace("/", " OR ")
    tokens = re.findall(r"\(|\)|[^\s()]+(?: WITH [^\s()]+)?", expr)
    pos = 0

    def parse_or():
        nonlocal pos
        alts = parse_and()
        while pos < len(tokens) and tokens[pos] == "OR":
            pos += 1
            alts = alts + parse_and()
        return alts

    def parse_and():
        nonlocal pos
        alts = parse_atom()
        while pos < len(tokens) and tokens[pos] == "AND":
            pos += 1
            right = parse_atom()
            alts = [a | b for a in alts for b in right]
        return alts

    def parse_atom():
        nonlocal pos
        tok = tokens[pos]
        pos += 1
        if tok == "(":
            alts = parse_or()
            pos += 1  # ")"
            return alts
        if tok not in SPDX:
            sys.exit(f"unknown SPDX license {tok!r}: add it to SPDX in {sys.argv[0]}")
        return [frozenset([SPDX[tok]])]

    return parse_or()


def main():
    ebuild = sys.argv[1]
    root = os.path.dirname(os.path.abspath(__file__)) + "/../.."
    with open(os.path.join(root, "Cargo.lock"), "rb") as f:
        lock = tomllib.load(f)
    crates = sorted(
        (p["name"], p["version"]) for p in lock["package"] if p.get("source", "").startswith("registry+")
    )
    registries = glob.glob(os.path.expanduser("~/.cargo/registry/src/*/"))
    required, choices, missing = set(), [], []
    for name, version in crates:
        manifest = next((os.path.join(r, f"{name}-{version}", "Cargo.toml") for r in registries if os.path.exists(os.path.join(r, f"{name}-{version}", "Cargo.toml"))), None)
        if manifest is None:
            missing.append(f"{name}-{version}")
            continue
        with open(manifest, "rb") as f:
            lic = tomllib.load(f).get("package", {}).get("license")
        if not lic:
            sys.exit(f"{name}-{version} has no SPDX license field; check it by hand")
        alts = alternatives(lic)
        if len(alts) == 1:
            required |= alts[0]
        else:
            choices.append(alts)
    # Licenses every alternative needs are required; an OR is already
    # satisfied when one alternative is required anyway.
    for alts in choices:
        required |= frozenset.intersection(*alts)
    rest = [alts for alts in choices if not any(a <= required for a in alts)]
    licenses = " ".join(sorted(required, key=str.lower))
    for alts in rest:
        group = " ".join(" ".join(sorted(a)) if len(a) == 1 else "( " + " ".join(sorted(a)) + " )" for a in alts)
        licenses += f" || ( {group} )"
    words, lines, cur = licenses.split(" "), [], ""
    for w in words:
        if len(cur) + len(w) > 70:
            lines.append(cur.strip())
            cur = ""
        cur += " " + w
    lines.append(cur.strip())

    text = open(ebuild).read()
    crate_block = "CRATES=\"\n" + "".join(f"\t{n}@{v}\n" for n, v in crates) + "\""
    text = re.sub(r'CRATES="[^"]*"', lambda _: crate_block, text, count=1)
    lic_block = 'LICENSE+="\n' + "".join(f"\t{l}\n" for l in lines) + '"'
    text = re.sub(r'LICENSE\+="[^"]*"', lambda _: lic_block, text, count=1)
    open(ebuild, "w").write(text)
    print(f"{len(crates)} crates, licenses: {licenses}")
    if missing:
        print(f"note: {len(missing)} crates are not in the local registry (other platforms); run `cargo fetch` to include their licenses:")
        print("  " + " ".join(missing))


if __name__ == "__main__":
    main()
