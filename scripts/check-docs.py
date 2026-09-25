#!/usr/bin/env python3
"""Check the published quickstart and prevent documented snippets/version drift."""

import json
import os
from pathlib import Path
import re
import subprocess
import tempfile
import tomllib


ROOT = Path(__file__).resolve().parents[1]


def blocks(document, language):
    return re.findall(rf"^```{re.escape(language)}\n(.*?)^```", document, re.M | re.S)


def normalized(source):
    # rustfmt can wrap imports/calls differently; compare all non-whitespace.
    return re.sub(r"\s+", "", source)


def main():
    readme = (ROOT / "README.md").read_text()
    guide = (ROOT / "docs/guide.md").read_text()
    manifest = tomllib.loads((ROOT / "Cargo.toml").read_text())
    msrv = manifest["workspace"]["package"]["rust-version"]
    # The guide targets a published release, independently of an upcoming
    # workspace version. Bumping the crate must not require publishing first.
    version = re.search(r"\*\*octoevents (\d+\.\d+\.\d+)\*\*", readme).group(1)
    dependencies = blocks(readme, "toml")
    programs = blocks(readme, "rust,no_run")
    assert len(dependencies) == len(programs) == 1, "expected one quickstart"
    requirement = tomllib.loads(dependencies[0])["dependencies"]["octoevents"]["version"]
    assert requirement == ".".join(version.split(".")[:2]), "quickstart version drift"
    assert f"**{msrv}**" in readme, "README MSRV drift"
    for page in [ROOT / "README.md", *sorted((ROOT / "docs").glob("*.md"))]:
        for linked_version in re.findall(r"https://docs.rs/octoevents/([^/]+)/", page.read_text()):
            assert linked_version == version, f"{page}: reference version drift"

    tests = normalized((ROOT / "tests/readme_testing.rs").read_text())
    ignored = blocks(guide, "rust,ignore")
    assert len(ignored) == 2, "new ignored snippet needs executable coverage"
    for snippet in ignored:
        body = snippet[snippet.index("#[tokio::test]"):].strip()
        assert normalized(body) in tests, "guide test differs from executable companion"

    # Compile the exact advertised dependency requirement first. A local path
    # substitution alone would miss regressions in the published install path.
    with tempfile.TemporaryDirectory(prefix="octoevents-docs-") as directory:
        consumer = Path(directory)
        (consumer / "src").mkdir()
        (consumer / "src/main.rs").write_text(programs[0])
        package = '[package]\nname = "docs-consumer"\nversion = "0.0.0"\nedition = "2024"\n'
        (consumer / "Cargo.toml").write_text(package + dependencies[0])
        env = os.environ.copy()
        env.setdefault("CARGO_TARGET_DIR", str(ROOT / "target/docs-consumer"))
        subprocess.run(["cargo", "check"], cwd=consumer, env=env, check=True)
        # A direct path cannot silently lose to a newer compatible registry
        # version, as a crates.io patch could. Its derive dependency is local too.
        local_dependencies = dependencies[0].replace(
            f'octoevents = {{ version = "{requirement}"',
            f'octoevents = {{ path = {json.dumps(str(ROOT))}',
        )
        (consumer / "Cargo.toml").write_text(package + local_dependencies)
        subprocess.run(["cargo", "update", "-p", "octoevents"], cwd=consumer, env=env, check=True)
        subprocess.run(["cargo", "check", "--locked"], cwd=consumer, env=env, check=True)
    print("Documentation versions, test snippets, and both quickstart builds passed.")


if __name__ == "__main__":
    main()
