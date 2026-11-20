# Experiments

Track: Databricks Shopping Sessions (DuckDB). rules.md wins over the starter. Exact match required, then fastest.

| Version | Change | Local result | Leaderboard | Notes |
| --- | --- | --- | --- | --- |
| v1 | Correctness baseline: raw line reader (read_text + split on LF), SQL parsing, scipy union-find for links, ASOF credit, range-join assists | check-dev MATCH (117,038 / 27,400 rows), 36/36 fixtures, 167 unit tests, 1M run 6.9 s in the amd64 image (emulated on a Mac, not a real timing) | not submitted | Events parse+dedup is the slow part (5M events: 23 s of 30 s). Next step is speed, after submission of this baseline. |
