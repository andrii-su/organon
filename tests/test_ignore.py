"""Keep Python indexer ignores aligned with the filesystem scanner."""

from ai.common.ignore import is_ignored


def test_build_artifacts_are_ignored() -> None:
    for path in [
        "/tmp/work/HomelabKit/.build/debug/Model.d",
        "/tmp/work/HomelabKit/.swiftpm/configuration/registries.json",
        "/tmp/work/src/lacon.egg-info/PKG-INFO",
    ]:
        assert is_ignored(path)
    assert not is_ignored("/tmp/work/src/models.py")
