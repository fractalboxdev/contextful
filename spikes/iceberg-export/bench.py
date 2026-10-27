"""Drives the probe Worker for the #734 query matrix. Latency is the Worker's own
measurement of each Basin SQL request; this script only schedules calls and aggregates.

  PROBE_URL=https://<worker host> PROBE_CRED=<audience credential> python3 bench.py <mode> ...

  matrix <runs> <out.json> [table:kind ...]   runs sequential samples per cell
  load <kind> <table> <concurrency> <per_client> <out.json>
  once <kind> <table>                          one query, e.g. after an idle period
  lag <table> <epoch> -- <exporter command>    export, then poll Basin SQL (BASIN_SQL_URL,
                                               BASIN_SQL_TOKEN) until the epoch is readable
"""

import json
import os
import statistics
import sys
import threading
import time
import urllib.error
import urllib.parse
import urllib.request

TABLES = ["small", "medium", "wide_files"]
KINDS = ["point", "range", "groupby", "join"]


def probe(kind, table, n, budget_ms=60_000):
    """One Worker call; a dropped client connection is retried, and counts in `retries`."""
    for attempt in range(5):
        try:
            return _probe(kind, table, n, budget_ms)
        except (ConnectionError, TimeoutError, urllib.error.URLError) as e:
            RETRIES.append(f"{kind}:{table}: {e}")
            time.sleep(2 ** attempt)
    return _probe(kind, table, n, budget_ms)


RETRIES = []


def _probe(kind, table, n, budget_ms):
    q = urllib.parse.urlencode({"kind": kind, "table": table, "n": n, "budget_ms": budget_ms})
    req = urllib.request.Request(
        f"{os.environ['PROBE_URL']}/probe?{q}",
        headers={"Authorization": f"Bearer {os.environ['PROBE_CRED']}", "User-Agent": "bench"},
    )
    with urllib.request.urlopen(req, timeout=900) as res:
        return json.load(res)


def pct(xs, p):
    xs = sorted(xs)
    return xs[min(len(xs) - 1, max(0, -(-p * len(xs) // 100) - 1))] if xs else None


def summarize(samples):
    ok = [s for s in samples if s["status"] == 200]
    ms = [s["ms"] for s in ok]
    med = lambda k: statistics.median(s["metrics"][k] for s in ok) if ok else None
    return {
        "n": len(ok),
        "errors": len(samples) - len(ok),
        "error_sample": next((s.get("error") for s in samples if s["status"] != 200), None),
        "p50": pct(ms, 50), "p95": pct(ms, 95), "p99": pct(ms, 99),
        "min": min(ms, default=None), "max": max(ms, default=None),
        "bytes_scanned": med("bytes_scanned"), "files_scanned": med("files_scanned"),
        "r2_requests": med("r2_requests_count"), "cache_hits": med("cache_hits"),
    }


def matrix(runs, out, cells):
    results = json.load(open(out)) if os.path.exists(out) else {}
    for cell in cells:
        if cell in results and results[cell]["n"] >= runs:
            continue
        table, kind = cell.split(":")
        samples, colo = [], None
        while len([s for s in samples if s["status"] == 200]) < runs and len(samples) < runs * 2:
            r = probe(kind, table, min(40, runs - len(samples)))
            samples += r["samples"]
            colo = r.get("colo")
        results[cell] = {**summarize(samples), "colo": colo, "client_retries": len(RETRIES)}
        RETRIES.clear()
        json.dump(results, open(out, "w"), indent=1)
        print(cell, json.dumps(results[cell]), flush=True)


def load(kind, table, c, per_client, out):
    samples, lock = [], threading.Lock()

    def client():
        r = probe(kind, table, per_client, budget_ms=900_000)
        with lock:
            samples.extend(r["samples"])

    threads = [threading.Thread(target=client) for _ in range(c)]
    t0 = time.monotonic()
    for t in threads:
        t.start()
    for t in threads:
        t.join()
    wall = time.monotonic() - t0
    s = summarize(samples)
    res = {"kind": kind, "table": table, "concurrency": c, "wall_s": round(wall, 2),
           "qps": round(s["n"] / wall, 3), **s}
    results = json.load(open(out)) if os.path.exists(out) else {}
    results[f"{table}:{kind}:c{c}"] = res
    json.dump(results, open(out, "w"), indent=1)
    print(json.dumps(res), flush=True)


def basin_sql(sql):
    req = urllib.request.Request(
        os.environ["BASIN_SQL_URL"],
        data=json.dumps({"query": sql}).encode(),
        headers={"Authorization": f"Bearer {os.environ['BASIN_SQL_TOKEN']}",
                 "Content-Type": "application/json", "User-Agent": "bench"},
    )
    with urllib.request.urlopen(req, timeout=300) as res:
        return json.load(res)["result"]["rows"]


def lag(table, epoch, exporter):
    """Runs the exporter, then polls Basin SQL until the new epoch is readable."""
    import subprocess
    t0 = time.monotonic()
    out = subprocess.run(exporter, capture_output=True, text=True, check=True).stdout
    t_export = time.monotonic() - t0
    polls = 0
    while True:
        polls += 1
        rows = basin_sql(f"SELECT epoch FROM bench.{table} WHERE key = 0")
        if rows and rows[0]["epoch"] == epoch:
            break
        time.sleep(0.25)
    t_readable = time.monotonic() - t0
    print(json.dumps({"table": table, "epoch": epoch, "export_s": round(t_export, 2),
                      "readable_s": round(t_readable, 2), "polls": polls,
                      "exporter": json.loads(out.strip().splitlines()[-1])}))


if __name__ == "__main__":
    mode, args = sys.argv[1], sys.argv[2:]
    if mode == "matrix":
        cells = args[2:] or [f"{t}:{k}" for t in TABLES for k in KINDS]
        matrix(int(args[0]), args[1], cells)
    elif mode == "load":
        load(args[0], args[1], int(args[2]), int(args[3]), args[4])
    elif mode == "lag":
        lag(args[0], int(args[1]), args[3:] if args[2] == "--" else args[2:])
    elif mode == "once":
        t0 = time.monotonic()
        r = probe(args[0], args[1], 1)
        print(json.dumps({"at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
                          "client_s": round(time.monotonic() - t0, 2), **r["samples"][0]}))
