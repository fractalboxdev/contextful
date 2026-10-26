-- Reads the exported table through the REST catalog with DuckDB's iceberg extension, an
-- engine independent of the writer. Run: duckdb -c ".read verify.sql"
INSTALL iceberg; LOAD iceberg;
ATTACH 'warehouse' AS cat (TYPE iceberg, ENDPOINT 'http://localhost:18181', AUTHORIZATION_TYPE 'none');
SELECT count(*) AS rows, min(id) AS min_id, max(id) AS max_id FROM cat.audience_a.events_v2;
SELECT id, hits, embedding, amount, label, observed_at, epoch_ns(observed_at) % 1000 AS ns_digit
FROM cat.audience_a.events_v2 WHERE id = 42;
SELECT label, count(*) AS n, sum(amount) AS total FROM cat.audience_a.events_v2 GROUP BY label ORDER BY label LIMIT 3;
DESCRIBE cat.audience_a.events_v2;
SELECT snapshot_id, sequence_number FROM iceberg_snapshots(cat.audience_a.events_v2);
