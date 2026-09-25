#!/usr/bin/env python3
"""Check the quickstart installation commands and executable guide snippets."""

import os
from pathlib import Path
import re
import shlex
import subprocess
import tempfile


ROOT = Path(__file__).resolve().parents[1]


def blocks(document, language):
    return re.findall(rf"^```{re.escape(language)}\n(.*?)^```", document, re.M | re.S)


def normalized(source):
    # rustfmt can wrap imports/calls differently; compare all non-whitespace.
    return re.sub(r"\s+", "", source)


def main():
    readme = (ROOT / "README.md").read_text()
    guide = (ROOT / "docs/guide.md").read_text()
    installation = [block for block in blocks(readme, "console") if block.startswith("cargo add ")]
    programs = blocks(readme, "rust,no_run")
    assert len(installation) == len(programs) == 1, "expected one quickstart"
    commands = [shlex.split(line) for line in installation[0].splitlines() if line.strip()]
    assert all(command[:2] == ["cargo", "add"] for command in commands)

    tests = normalized((ROOT / "tests/readme_testing.rs").read_text())
    ignored = blocks(guide, "rust,ignore")
    assert len(ignored) == 2, "new ignored snippet needs executable coverage"
    for snippet in ignored:
        body = snippet[snippet.index("#[tokio::test]"):].strip()
        assert normalized(body) in tests, "guide test differs from executable companion"

    # Run the exact installation commands first. A local path
    # substitution alone would miss regressions in the published install path.
    with tempfile.TemporaryDirectory(prefix="octoevents-docs-") as directory:
        consumer = Path(directory)
        (consumer / "src").mkdir()
        (consumer / "src/main.rs").write_text(programs[0])
        package = '[package]\nname = "docs-consumer"\nversion = "0.0.0"\nedition = "2024"\n'
        (consumer / "Cargo.toml").write_text(package)
        env = os.environ.copy()
        env.setdefault("CARGO_TARGET_DIR", str(ROOT / "target/docs-consumer"))
        for command in commands:
            subprocess.run(command, cwd=consumer, env=env, check=True)
        subprocess.run(["cargo", "check"], cwd=consumer, env=env, check=True)
        # A direct path cannot silently lose to a newer compatible registry
        # version, as a crates.io patch could. Its derive dependency is local too.
        subprocess.run(
            ["cargo", "add", "octoevents", "--path", str(ROOT)],
            cwd=consumer, env=env, check=True,
        )
        subprocess.run(["cargo", "check", "--locked"], cwd=consumer, env=env, check=True)
    print("Installation commands, test snippets, and both quickstart builds passed.")


if __name__ == "__main__":
    main()
