"""Pinned multilingual embeddings for the generic Apache-2.0 semantic corpus."""
import hashlib
import json
from pathlib import Path

import torch
from huggingface_hub import snapshot_download
from sentence_transformers import SentenceTransformer

ROOT = Path(__file__).resolve().parent
MODEL = "sentence-transformers/paraphrase-multilingual-MiniLM-L12-v2"
REVISION = "e8f8c211226b894fcb81acc59f3b34ba3efd5f42"


def write_json(path, value):
    path.write_text(json.dumps(value, ensure_ascii=False, indent=2) + "\n")


def write_lines(path, rows):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text("".join(json.dumps(row, ensure_ascii=False, separators=(",", ":")) + "\n" for row in rows))


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main():
    torch.set_num_threads(1)
    torch.manual_seed(683)
    torch.use_deterministic_algorithms(True)
    source = json.loads((ROOT / "source.json").read_text())
    snapshot = Path(snapshot_download(MODEL, revision=REVISION, allow_patterns=["*.json", "*.safetensors", "tokenizer.model", "vocab.txt", "1_Pooling/*"]))
    model = SentenceTransformer(str(snapshot), device="cpu")
    rows, queries = [], []
    for topic in source["topics"]:
        rows.extend([{"doc_id": topic["id"], "text": topic["text"]}, {"doc_id": topic["id"] + "-negative", "text": topic["negative"]}])
        document_language = "ja" if topic["id"] in ["delivery", "library", "train"] else "en"
        for kind, language in [("en", "en"), ("ja", "ja"), ("alias", "en")]:
            tags = ["heldout", "hard_negative", language, "alias" if kind == "alias" else "paraphrase"]
            if language != document_language:
                tags.append("cross_language")
            queries.append({"id": topic["id"] + "-" + kind, "corpus": "corpus", "prefix": "kb/semantic", "question": topic[kind], "tags": tags, "expected": {"artifacts": ["kb/semantic#" + topic["id"]]}})
    rows.extend({"doc_id": f"distractor-{i}", "text": text} for i, text in enumerate(source["distractors"]))
    vectors = model.encode([row["text"] for row in rows] + [q["question"] for q in queries], batch_size=1, normalize_embeddings=True, show_progress_bar=True).tolist()
    for row, vector in zip(rows, vectors):
        row["embedding"] = vector
    for query, vector in zip(queries, vectors[len(rows):]):
        query["query_embedding"] = vector
    write_lines(ROOT / "corpus/rows/kb/semantic.jsonl", rows)
    write_lines(ROOT / "heldout.jsonl", queries)
    # Development prompts have disjoint ids and wording; held-out prompts determine no tuning.
    write_lines(ROOT / "development.jsonl", [{"id": "dev-" + t["id"], "question": t["text"], "relevant": "kb/semantic#" + t["id"]} for t in source["topics"][:4]])
    config = {"k": 10, "fusion": "reciprocal-rank", "normalize_embeddings": True, "max_seq_length": model.max_seq_length, "batch_size": 1, "device": "cpu", "seed": 683, "regression_tolerance": 0.02}
    write_json(ROOT / "config.json", config)
    files = ["source.json", "regenerate.py", "requirements.txt", "config.json", "development.jsonl", "heldout.jsonl", "corpus/contextful.toml", "corpus/rows/kb/semantic.jsonl"]
    artifacts = {str(p.relative_to(snapshot)): digest(p) for p in sorted(snapshot.rglob("*")) if p.is_file()}
    write_json(ROOT / "manifest.json", {"version": "semantic-v1", "model": MODEL, "revision": REVISION, "license": "Apache-2.0", "dimension": 384, "heldout": "heldout.jsonl", "files": {name: digest(ROOT / name) for name in files}, "model_artifacts": artifacts})


if __name__ == "__main__":
    main()
