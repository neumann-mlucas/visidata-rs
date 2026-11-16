# S-42 — Readers and aggregators the replay corpus is blocked on

Status: **in progress 2026-11-16**.
Depends on: S-03 (readers), S-25 P4 (aggregators), S-22 (unfurl).

## 1. Today

Replay: `PASS=75 FAIL=0 SKIP=109 UNTESTABLE=20 QUARANTINED=11 NO_GOLDEN=25`. 41 skips stop at
`open-file` with `unknown format`, two at a JSONL comment line, two at a missing aggregator, two
at the `unfurl_empty` option. Each part is a real reader / aggregator / option, then the replay
translation.

## 2. Parts (one commit each)

| Part | What | VisiData | Replay files |
|---|---|---|---|
| A | JSONL: blank lines and `#` / `//` lines are skipped (`JsonSheet.options.regex_skip`) | `loaders/json.py` | fill-nested, unfurl-list |
| B | Readers: fixed width (`--format fixed`), `.usv`, `.lsv`, `.jsonla`; replay honours `set-option filetype` | `loaders/fixed_width.py`, `usv.py`, `lsv.py`, `json.py` | load-fixed, pr2400, load-usv, load-lsv, load-jsonla |
| C | Aggregators `p10 p20 p25 p30 p33 p40 p50 p60 p67 p70 p75 p80 p90 p95 p99` (linear interpolation); `q3 q4 q5 q10` add their percentiles; option `unfurl_empty` | `aggregators.py`, `features/unfurl.py` | test_quartiles, unfurl-empty |
| D | Readers with a crate: `.yaml/.yml`, `.toml`, `.html/.htm` (tables list + links), `.zip` (members list) | `loaders/yaml.py`, `toml.py`, `html.py`, `archive.py` | load-yaml, load-toml, issue1308, issue1346, issue2227, pull2140, load-html, load-zip |

Formats read eagerly into text columns, as VisiData does (no type guessing beyond JSON's own
types); `@ # %` type them later.

## 3. Out of scope

- **Error cells.** A bad JSONL line is a `#ERR` row in VisiData; vdrs has no error cell value
  (a failed cast shows empty). 7 files (`test.jsonl`: 1410b, describe-error, melt-error,
  save-json, aggregators-errors, freq-error, pivot-error) test how `#ERR` flows through freq,
  pivot, describe and save. Needs its own spec.
- `keymin` / `keymax`: an aggregator over the key columns; the aggregator expr takes one column.
  One file (pr2308), which also needs `freq-summary`.
- Fixed width with `header 0` (load-fixed-header0, load-fixed-leadingspaces): every column is
  named `''`, and a DataFrame can't hold two columns of one name.
- XML (load-xml, xml-table): VisiData shows an element tree and dives into it
  (`dive-xml-table`); a different sheet kind.
