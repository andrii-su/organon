"""Local vector store using fastembed + lancedb."""
from pathlib import Path


def get_store(db_path: str = "~/.organon/vectors"):
    try:
        import lancedb
        path = Path(db_path).expanduser()
        path.mkdir(parents=True, exist_ok=True)
        return lancedb.connect(str(path))
    except ImportError:
        raise ImportError("Install lancedb: pip install lancedb")


def embed_text(text: str) -> list[float]:
    try:
        from fastembed import TextEmbedding
        model = TextEmbedding()
        embeddings = list(model.embed([text]))
        return embeddings[0].tolist()
    except ImportError:
        raise ImportError("Install fastembed: pip install fastembed")
