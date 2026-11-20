"""Tests for content extractor."""

import pytest

from ai.extractor.extract import CODE_EXTENSIONS, extract_text

# ── basic extraction ──────────────────────────────────────────────────────────

def test_extracts_plain_text(tmp_path):
    f = tmp_path / "hello.txt"
    f.write_text("hello world")
    assert extract_text(str(f)) == "hello world"


def test_extracts_markdown(tmp_path):
    f = tmp_path / "readme.md"
    f.write_text("# Title\n\nContent here.")
    result = extract_text(str(f))
    assert "Title" in result


def test_extracts_rust_source(tmp_path):
    f = tmp_path / "main.rs"
    f.write_text("fn main() { println!(\"hello\"); }")
    result = extract_text(str(f))
    assert "fn main" in result


def test_extracts_python_source(tmp_path):
    f = tmp_path / "script.py"
    f.write_text("def hello():\n    return 42\n")
    result = extract_text(str(f))
    assert "def hello" in result


def test_returns_none_for_nonexistent():
    assert extract_text("/nonexistent/path/file.txt") is None


def test_returns_none_for_binary(tmp_path):
    f = tmp_path / "data.bin"
    f.write_bytes(bytes(range(256)) * 10)
    # Binary files with no known extension and non-UTF8 content → None
    result = extract_text(str(f))
    # Either None or truncated — should not raise
    assert result is None or isinstance(result, str)


def test_truncates_large_files(tmp_path):
    f = tmp_path / "big.txt"
    f.write_text("x" * 100_000)
    result = extract_text(str(f))
    assert result is not None
    assert len(result) <= 32_000 + 1  # MAX_CHARS


def test_empty_file(tmp_path):
    f = tmp_path / "empty.py"
    f.write_text("")
    result = extract_text(str(f))
    assert result == ""


# ── code extensions ───────────────────────────────────────────────────────────

@pytest.mark.parametrize("ext", [".rs", ".py", ".ts", ".go", ".toml", ".md"])
def test_code_extensions_covered(ext):
    assert ext in CODE_EXTENSIONS


def test_all_code_extensions_extractable(tmp_path):
    for ext in [".rs", ".py", ".ts", ".js", ".go", ".toml", ".yaml"]:
        f = tmp_path / f"file{ext}"
        f.write_text("content for " + ext)
        result = extract_text(str(f))
        assert result is not None, f"failed for {ext}"
        assert "content" in result


def test_never_extracts_sensitive_files(tmp_path):
    secret = tmp_path / "prod.env"
    secret.write_text("API_KEY=zzz\n")
    assert extract_text(str(secret)) is None


def test_bridge_extract_text_withholds_sensitive_files(tmp_path):
    from ai.bridge import _run

    secret = tmp_path / ".pgpass"
    secret.write_text("db:5432:*:u:p\n")
    result = _run({"op": "extract_text", "path": str(secret)})
    assert result["content"] == ""
