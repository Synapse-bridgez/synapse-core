#!/usr/bin/env python3
"""Fail when docs/openapi/swap.json changes in a breaking way without a major
version bump. Usage: check-openapi-breaking.py <base.json> <head.json>

Breaking: removed path/method/response code, newly-required parameter, or a
parameter that changed from optional to required. See docs/swap-api-versioning.md.
"""
import json
import sys

METHODS = {"get", "post", "put", "patch", "delete"}


def load(p):
    with open(p) as f:
        return json.load(f)


def major(spec):
    return int(spec["info"]["version"].split(".")[0])


def breaking(base, head):
    out = []
    for path, ops in base.get("paths", {}).items():
        hops = head.get("paths", {}).get(path)
        if hops is None:
            out.append(f"removed path {path}")
            continue
        for m in METHODS & ops.keys():
            if m not in hops:
                out.append(f"removed {m.upper()} {path}")
                continue
            for code in ops[m].get("responses", {}):
                if code not in hops[m].get("responses", {}):
                    out.append(f"removed response {code} from {m.upper()} {path}")
            old = {p["name"]: p for p in ops[m].get("parameters", [])}
            for p in hops[m].get("parameters", []):
                was = old.get(p["name"])
                if p.get("required") and (was is None or not was.get("required")):
                    out.append(f"parameter '{p['name']}' became required on {m.upper()} {path}")
    return out


def main(base_p, head_p):
    base, head = load(base_p), load(head_p)
    problems = breaking(base, head)
    if problems and major(head) <= major(base):
        print("Breaking OpenAPI changes without a major version bump:")
        for p in problems:
            print(f"  - {p}")
        print(f"info.version is {head['info']['version']} (base {base['info']['version']}); bump the major and add a /swap/v{{N+1}} prefix.")
        return 1
    print("OpenAPI compatibility check passed" + (" (major bump)" if problems else ""))
    return 0


if __name__ == "__main__":
    sys.exit(main(*sys.argv[1:3]))
