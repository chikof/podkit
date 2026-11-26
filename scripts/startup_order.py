#!/usr/bin/env python3
"""The start-up symbol order of the server binary (docs/footprint.md, "Start-up order").

  startup_order.py profile CALLGRIND_OUT BINARY LINK_MAP > server/startup.order
      The functions of BINARY that a callgrind run executed, one key per line.
  startup_order.py resolve LINK_MAP server/startup.order > ORDER_FILE
      The symbols of this build whose key is listed, for lld's --symbol-ordering-file.

A key is a hash of the demangled name, which has no per-crate hash in it, so the list
survives a dependency bump or a new compiler. Both sides take the demangled name from
lld's link map: a mangled name cannot be compared across builds, because its internal
back-references move when a crate hash changes length.

axum's per-route glue also gets a key without its generic arguments, so the glue of a
route added after the list was made is ordered with the rest.
"""
import hashlib
import re
import sys

SECTION = re.compile(r"^\s*[0-9a-f]+\s+[0-9a-f]+\s+[0-9a-f]+\s+\d+ {9}\S.*:\(\.text\.(\S+)\)$")
SYMBOL = re.compile(r"^\s*[0-9a-f]+\s+[0-9a-f]+\s+[0-9a-f]+\s+\d+ {17}(\S.*)$")


def keys(demangled):
    """The exact key and, for axum's glue, the key of the generic function it is an instance of."""
    out = [hashlib.sha256(demangled.encode()).hexdigest()[:16]]
    generic, n = demangled.replace("->", "\0"), 1
    while n:
        # Generic arguments follow a path; the `<` of `<T as Trait>` does not.
        generic, n = re.subn(r"(?<=[\w:])<[^<>]*>", "", generic)
    if "axum::" in generic:
        out.append(hashlib.sha256(generic.encode()).hexdigest()[:16])
    return out


def sections(link_map):
    """(mangled name of a function section, demangled names of its symbols)."""
    mangled, out = None, {}
    for line in open(link_map):
        if m := SECTION.match(line):
            mangled = m.group(1)
            out[mangled] = []
        elif mangled and (m := SYMBOL.match(line)):
            out[mangled].append(m.group(1))
        else:
            mangled = None
    return out


def profile(callgrind_out, binary, link_map):
    names, objects, current, executed = {}, {}, None, set()
    for line in open(callgrind_out):
        m = re.match(r"(c?ob|c?fn)=\((\d+)\)(?: (.*))?$", line)
        if not m:
            continue
        kind, ident, name = m.groups()
        if kind.endswith("ob"):
            if name:
                objects[ident] = name
            if kind == "ob":
                current = objects[ident]
        else:
            if name:
                # callgrind numbers the contexts of a recursive function: "name'2".
                names[ident] = name.split("'")[0]
            if kind == "fn" and current == binary:
                executed.add(names[ident])
    known = sections(link_map)
    listed = {k for n in executed for d in known.get(n, ()) for k in keys(d)}
    assert len(listed) > 1000, f"only {len(listed)} keys for {len(executed)} executed functions"
    return sorted(listed)


def resolve(link_map, listed):
    wanted = set(open(listed).read().split())
    hits = sorted(m for m, names in sections(link_map).items() if any(k in wanted for d in names for k in keys(d)))
    print(f"startup order: {len(hits)} symbols for {len(wanted)} listed", file=sys.stderr)
    assert len(hits) > len(wanted) // 2, "the list is stale: regenerate it with scripts/startup-order.sh"
    return hits


if __name__ == "__main__":
    mode, *args = sys.argv[1:]
    print("\n".join({"profile": profile, "resolve": resolve}[mode](*args)))
