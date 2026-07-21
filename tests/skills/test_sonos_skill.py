"""Frontmatter validation tests for the sonos skill.

No soco-cli installation is required: these tests only parse and
validate the SKILL.md YAML frontmatter.
"""

from pathlib import Path

import pytest
import yaml

SKILL_MD = (
    Path(__file__).resolve().parent
    / ".."
    / ".."
    / "skills"
    / "smart-home"
    / "sonos"
    / "SKILL.md"
).resolve()


@pytest.fixture(scope="module")
def frontmatter():
    text = SKILL_MD.read_text(encoding="utf-8")
    assert text.startswith("---\n"), "SKILL.md must start with YAML frontmatter"
    end = text.index("\n---", 4)
    data = yaml.safe_load(text[4:end])
    assert isinstance(data, dict)
    return data


def test_skill_md_exists():
    assert SKILL_MD.is_file(), f"missing {SKILL_MD}"


def test_yaml_frontmatter_parses(frontmatter):
    assert frontmatter  # non-empty dict


def test_required_fields_present(frontmatter):
    for field in ("name", "description", "version", "author", "license", "platforms"):
        assert field in frontmatter, f"missing frontmatter field: {field}"


def test_name(frontmatter):
    assert frontmatter["name"] == "sonos"


def test_description_length_and_shape(frontmatter):
    desc = frontmatter["description"]
    assert isinstance(desc, str)
    assert len(desc) <= 60, f"description too long: {len(desc)} chars"
    assert desc.endswith("."), "description must end with a period"


def test_version_and_author(frontmatter):
    assert frontmatter["version"] == "0.1.0"
    assert frontmatter["author"] == "Hermes Agent"
    assert frontmatter["license"] == "MIT"


def test_platforms(frontmatter):
    platforms = frontmatter["platforms"]
    assert isinstance(platforms, list)
    assert set(platforms) == {"linux", "macos", "windows"}


def test_hermes_metadata(frontmatter):
    hermes = frontmatter.get("metadata", {}).get("hermes", {})
    assert hermes.get("tags") == ["Sonos", "Smart-Home", "Audio"]
    assert hermes.get("related_skills") == ["openhue"]
