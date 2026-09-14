#!/usr/bin/env python3
"""Particle-mode authoring aid for Xindeler's `voxygen` particle system.

A particle effect in this engine is a *number*. `ParticleMode` (Rust, in
`voxygen/src/render/pipelines/particle.rs`) is uploaded per instance as
`inst_mode`, and `assets/voxygen/shaders/particle-vert.glsl` switches on that
number to decide the particle's motion, size and colour over its lifetime.
Nothing connects the two sides but the integer, and every way of getting it
wrong is silent:

* a Rust mode with no `case` in the shader renders as the shader's `default:`
  arm — small white motes drifting upward — with no log line and no panic;
* a shader constant with no Rust mode is dead GLSL;
* reusing a number that is already taken silently repaints an existing effect.

`shader_modes_match_particle_modes` in
`voxygen/src/render/pipelines/particle.rs` is the authoritative gate — it
enumerates the real enum through `strum` instead of parsing it, and it also
compiles the GLSL. But it needs `voxygen` built, which from cold takes tens of
minutes. This script answers the mode half of the same question in well under a
second with no build at all, and additionally reports what a new effect's author
needs: the next free mode number, and which modes are declared but never
emitted.

The two are kept honest by construction: the exemption list is read out of the
Rust source rather than duplicated here.

Usage (from the repository root):

    python3 tools/particles/particle_modes.py            # report + invariant
    python3 tools/particles/particle_modes.py --quiet    # invariant only

Exit code is 0 when Rust and GLSL agree, 1 otherwise.
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
RUST = ROOT / "voxygen" / "src" / "render" / "pipelines" / "particle.rs"
GLSL = ROOT / "assets" / "voxygen" / "shaders" / "particle-vert.glsl"
VOXYGEN_SRC = ROOT / "voxygen" / "src"

RUST_ENUM = re.compile(r"pub enum ParticleMode \{(.*?)\n\}", re.DOTALL)
RUST_VARIANT = re.compile(r"^\s{4}(\w+) = (\d+),\s*$", re.MULTILINE)
# The single source of truth for the exemption list lives in the Rust test, so
# the two checkers cannot drift apart.
RUST_EXEMPT = re.compile(
    r"const MODES_WITHOUT_SHADER_CASE: &\[ParticleMode\] = &\[(.*?)\];", re.DOTALL
)
GLSL_CONST = re.compile(r"^const int (\w+) = (\d+);\s*$", re.MULTILINE)
GLSL_CASE = re.compile(r"^\s*case (\w+):", re.MULTILINE)


def read(path: Path) -> str:
    try:
        return path.read_text(encoding="utf-8")
    except OSError as error:
        sys.exit(f"cannot read {path.relative_to(ROOT)}: {error}")


def rust_modes() -> dict[int, str]:
    source = read(RUST)
    body = RUST_ENUM.search(source)
    if not body:
        sys.exit(f"{RUST.relative_to(ROOT)}: could not find `pub enum ParticleMode`")

    modes: dict[int, str] = {}
    for name, value in RUST_VARIANT.findall(body.group(1)):
        number = int(value)
        if number in modes:
            sys.exit(f"ParticleMode: {modes[number]} and {name} both use {number}")
        modes[number] = name

    # A variant written without an explicit discriminant (`NewMode,`) would be
    # invisible to `RUST_VARIANT` and would silently make this whole report
    # wrong — exactly the failure class the tool exists to prevent. Count the
    # variant-ish lines and refuse to continue if any went unparsed.
    candidates = [
        line
        for line in body.group(1).splitlines()
        if line.strip() and not line.strip().startswith("//")
    ]
    if len(candidates) != len(modes):
        sys.exit(
            f"{RUST.relative_to(ROOT)}: parsed {len(modes)} of {len(candidates)} "
            "`ParticleMode` variants. Every variant must carry an explicit "
            "`= N` discriminant; the GLSL side is bound to that number."
        )
    return modes


def exempt_modes() -> set[str]:
    """`MODES_WITHOUT_SHADER_CASE`, read from the Rust test so it cannot drift."""
    block = RUST_EXEMPT.search(read(RUST))
    if not block:
        sys.exit(
            f"{RUST.relative_to(ROOT)}: could not find `MODES_WITHOUT_SHADER_CASE`"
        )
    return set(re.findall(r"ParticleMode::(\w+)", block.group(1)))


def glsl_modes() -> tuple[dict[int, str], set[str]]:
    source = read(GLSL)
    constants: dict[int, str] = {}
    for name, value in GLSL_CONST.findall(source):
        number = int(value)
        if number in constants:
            sys.exit(f"{GLSL.name}: {constants[number]} and {name} both use {number}")
        constants[number] = name
    return constants, set(GLSL_CASE.findall(source))


def emitted_modes() -> set[str]:
    """Mode names actually constructed anywhere in the client."""
    names: set[str] = set()
    for path in sorted(VOXYGEN_SRC.rglob("*.rs")):
        if path == RUST:
            continue  # the enum's own declaration is not an emission
        names.update(re.findall(r"ParticleMode::(\w+)", read(path)))
    return names


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--quiet",
        action="store_true",
        help="only report problems, skip the authoring summary",
    )
    args = parser.parse_args()

    modes = rust_modes()
    exempt = exempt_modes()
    constants, cases = glsl_modes()
    problems: list[str] = []

    for number, name in sorted(modes.items()):
        if name in exempt:
            if number in constants:
                problems.append(
                    f"{name} ({number}) now has a shader constant; drop it from "
                    "MODES_WITHOUT_SHADER_CASE in the Rust test"
                )
            continue
        constant = constants.get(number)
        if constant is None:
            problems.append(
                f"{name} ({number}) has no `const int … = {number};` in {GLSL.name}; "
                "it renders as the shader's `default:` white motes"
            )
        elif constant not in cases:
            problems.append(
                f"{GLSL.name} declares `{constant} = {number}` for {name} but has no "
                f"`case {constant}:`; it renders as the `default:` white motes"
            )

    for number, constant in sorted(constants.items()):
        if number not in modes:
            problems.append(
                f"{GLSL.name} declares `{constant} = {number}` but no ParticleMode "
                "uploads that number"
            )

    if not args.quiet:
        emitted = emitted_modes()
        unused = sorted(
            f"{name} ({number})"
            for number, name in modes.items()
            if name not in emitted
        )
        next_free = max(modes) + 1 if modes else 0
        print(f"Rust ParticleMode variants : {len(modes)}")
        print(f"GLSL mode constants        : {len(constants)}")
        print(f"GLSL `case` labels         : {len(cases)}")
        print(f"Next free mode number      : {next_free}")
        print(
            "Declared but never emitted : "
            + (", ".join(unused) if unused else "(none)")
        )

    if problems:
        print("\nParticle mode mismatch:", file=sys.stderr)
        for problem in problems:
            print(f"- {problem}", file=sys.stderr)
        return 1

    if not args.quiet:
        print("\nRust and GLSL particle modes agree.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
