"""Content extraction from files."""
import mimetypes
from pathlib import Path


def extract_text(path: str) -> str | None:
    """Extract text content from a file based on its type."""
    p = Path(path)
    if not p.exists():
        return None

    mime, _ = mimetypes.guess_type(path)

    if mime and mime.startswith("text/"):
        return p.read_text(errors="ignore")

    if p.suffix == ".pdf":
        return _extract_pdf(path)

    if p.suffix in {".md", ".rst", ".txt"}:
        return p.read_text(errors="ignore")

    return None


def _extract_pdf(path: str) -> str | None:
    try:
        import pypdf
        reader = pypdf.PdfReader(path)
        return "\n".join(page.extract_text() or "" for page in reader.pages)
    except ImportError:
        return None
