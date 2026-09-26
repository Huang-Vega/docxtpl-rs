#!/usr/bin/env python3
"""Small live Jinja2 oracle used by the Rust table-driven differential test."""

import json
import sys

from jinja2 import Environment


def main() -> None:
    cases = json.load(sys.stdin)
    results = []
    for case in cases:
        try:
            env = Environment(autoescape=bool(case.get("autoescape", False)))
            output = env.from_string(case["template"]).render(case.get("context", {}))
            results.append({"id": case["id"], "output": output, "error": None})
        except Exception as error:  # noqa: BLE001 - exception class is oracle data
            results.append(
                {"id": case["id"], "output": None, "error": type(error).__name__}
            )
    # ASCII JSON avoids depending on the host console code page (notably GBK on Windows).
    json.dump(results, sys.stdout, ensure_ascii=True)


if __name__ == "__main__":
    main()
