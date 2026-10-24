# Semantic retrieval quality

`v1` contains 36 generic documents, 36 held-out queries and four development queries under Apache-2.0. Twelve topics each pair a relevant document with a hard negative; twelve unrelated documents complete the corpus. The held-out queries cover paraphrases, aliases, English, Japanese and both directions of cross-language retrieval. Relevance is binary and belongs to each case's `expected.artifacts` field. Development prompts have disjoint ids and wording; held-out scores determine no embedding or retrieval configuration.

The [multilingual MiniLM model](https://huggingface.co/sentence-transformers/paraphrase-multilingual-MiniLM-L12-v2/tree/e8f8c211226b894fcb81acc59f3b34ba3efd5f42) supplies real 384-dimensional embeddings at revision `e8f8c211226b894fcb81acc59f3b34ba3efd5f42`, under Apache-2.0. `manifest.json` pins each input and cache with SHA-256, including model weight, tokenizer and pooling configuration digests. `config.json` fixes CPU inference, a single-item batch, unit normalization, the seed, sequence length, retrieval cutoff, fusion and regression tolerance. `requirements.txt` records the exact embedding dependencies. Model weights remain outside the repository; normal evaluation reads the committed cache without Python, model downloads or an inference service.

Run the existing ranked-store adapter with a credential admitting `kb/semantic` in `local:device`:

```sh
contextful eval run --goldens evals/semantic/v1/heldout.jsonl \
  --semantic-manifest evals/semantic/v1/manifest.json \
  --zone local:device --public-key <issuer-public-key> \
  --audience contextful://eval --report semantic-report.json
```

`CONTEXTFUL_TOKEN` supplies the signed credential. Each question reaches the real ingestion, fold, authorization and ranked retrieval paths three times: lexical, vector and hybrid, against identical rows and labels. The semantic tier bypasses answer reading, answer judging and deterministic mechanics floors. Safety and mechanics retain their separate native suite.

The report provides nDCG@10, Recall@10 and MRR, plus R-precision and hit rate, with per-metric sample counts, case-bootstrap 95% intervals and every named slice. An empty result is a miss. MRR's ranking contains at most ten results, so it is truncated at ten. Queries sharing a topic are correlated; case resampling treats them as separate observations and does not establish population confidence beyond this small authored corpus.

Add `--semantic-baseline evals/semantic/v1/baseline.json` to compare every overall and slice nDCG, recall and reciprocal-rank mean. A loss exceeding the pinned absolute tolerance produces a regression record and a failing exit code. A changed model, corpus, labels, generator, dependency freeze or configuration changes the benchmark identity and rejects the comparison. A cache revision uses a separate version directory and a measured baseline for that revision.

Regenerate embeddings with Python 3.12 and the pinned dependencies:

```sh
python3.12 -m venv .tmp/semantic-embeddings
.tmp/semantic-embeddings/bin/pip install -r evals/semantic/v1/requirements.txt
.tmp/semantic-embeddings/bin/python evals/semantic/v1/regenerate.py
```

The regeneration downloads only the immutable model revision, uses its mean pooling, computes unit-normalized vectors and writes cache and manifest hashes. Floating-point kernels can differ across CPU and library builds; the checked-in cache is the authoritative offline input. The benchmark measures the configured retrieval implementation on synthetic examples, not performance on a production collection or general multilingual search quality.
