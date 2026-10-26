# /// script
# requires-python = ">=3.11"
# dependencies = ["pyiceberg[pyarrow]==0.10.0"]
# ///
"""Reads the exported table through the REST catalog with PyIceberg, an engine
independent of the writer. Run: uv run verify.py <table>"""

import sys

from pyiceberg.catalog.rest import RestCatalog

name = sys.argv[1] if len(sys.argv) > 1 else "events_v3"
cat = RestCatalog("local", uri="http://localhost:18181")
t = cat.load_table(("audience_a", name))
print("format-version", t.metadata.format_version)
print(t.schema())
print("snapshots", [(s.snapshot_id, s.summary.operation.value) for s in t.snapshots()])
tbl = t.scan().to_arrow()
print("rows", tbl.num_rows)
row = t.scan(row_filter="id == 42").to_arrow()
import pyarrow as pa
print({c: (row[c].cast(pa.int64()) if c == "observed_at" else row[c]).to_pylist() for c in row.column_names})
print("arrow types", [str(f.type) for f in tbl.schema])
