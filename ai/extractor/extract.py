"""Content extraction from files."""

import logging
import mimetypes
from pathlib import Path

logger = logging.getLogger(__name__)

# Code extensions treated as plain text
CODE_EXTENSIONS = {
    ".py",
    ".rs",
    ".ts",
    ".tsx",
    ".js",
    ".jsx",
    ".go",
    ".java",
    ".c",
    ".cpp",
    ".h",
    ".hpp",
    ".cs",
    ".rb",
    ".php",
    ".swift",
    ".kt",
    ".sh",
    ".bash",
    ".zsh",
    ".fish",
    ".toml",
    ".yaml",
    ".yml",
    ".json",
    ".xml",
    ".sql",
    ".graphql",
    ".md",
    ".rst",
    ".txt",
    ".css",
    ".scss",
    ".html",
    ".dockerfile",
    ".makefile",
}

MAX_CHARS = 32_000  # ~8k tokens — enough for embeddings


def extract_text(path: str) -> str | None:
    """Extract text content from a file. Returns None if not extractable."""
    p = Path(path)
    if not p.exists() or not p.is_file():
        logger.debug("not found or not a file: %s", path)
        return None

    suffix = p.suffix.lower()

    if suffix in CODE_EXTENSIONS:
        logger.debug("code file [%s]: %s", suffix, path)
        return _read_text(p)

    mime, _ = mimetypes.guess_type(path)
    if mime and mime.startswith("text/"):
        logger.debug("text MIME [%s]: %s", mime, path)
        return _read_text(p)

    if suffix == ".pdf":
        logger.debug("PDF: %s", path)
        return _extract_pdf(path)

    # Fallback: try UTF-8 read for small extensionless files (Dockerfile, Makefile, etc.)
    if p.stat().st_size < 1_000_000:
        try:
            content = p.read_text(errors="strict", encoding="utf-8")
            logger.debug("utf-8 fallback: %s", path)
            return content[:MAX_CHARS]
        except (UnicodeDecodeError, PermissionError):
            logger.debug("binary or permission error: %s", path)

    logger.debug("not extractable: %s", path)
    return None


def _read_text(p: Path) -> str | None:
    try:
        content = p.read_text(errors="ignore")
        if len(content) > MAX_CHARS:
            logger.debug("truncated %d→%d chars: %s", len(content), MAX_CHARS, p)
        return content[:MAX_CHARS]
    except (PermissionError, OSError) as e:
        logger.warning("read error for %s: %s", p, e)
        return None


def _extract_pdf(path: str) -> str | None:
    try:
        import pypdf

        reader = pypdf.PdfReader(path)
        text = "\n".join(page.extract_text() or "" for page in reader.pages)
        logger.debug("PDF extracted %d chars: %s", len(text), path)
        return text[:MAX_CHARS]
    except ImportError:
        logger.warning("pypdf not installed, cannot extract PDF: %s", path)
        return None
    except Exception as e:
        logger.warning("PDF extraction failed for %s: %s", path, e)
        return None
