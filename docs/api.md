# CLI reference: `s2r`

`s2r` has no HTTP API. This page specifies its command-line interface.

```
s2r [GLOBAL OPTIONS] <COMMAND>
```

## Global options

| Option | Env | Default | Meaning |
|---|---|---|---|
| `--data-dir <DIR>` | `S2R_DATA_DIR` | `data` | Bronze CSVs, warehouse, LLM candidates |
| `--warehouse <FILE>` | `S2R_WAREHOUSE` | `<data-dir>/warehouse.duckdb` | DuckDB file built by `dbt build` |
| `--analytics-dir <DIR>` | `S2R_ANALYTICS_DIR` | `analytics` | dbt + MetricFlow project |
| `--mf-bin <FILE>` | `S2R_MF_BIN` | `<analytics-dir>/.venv/{bin,Scripts}/mf` | MetricFlow CLI |

An optional `.env` file in the working directory is loaded first (see `app/.env.example`).
Real environment variables take precedence.

Exit status: `0` on success, `1` on error (the message is printed to stderr, with its cause chain).

## Commands

### `seed`

```
s2r seed [--seed <N>]          # default 42
```

Writes `<data-dir>/bronze/*.csv`, the reference CSVs exported from the curated dictionary
(`ref_drug_condition.csv`, `ref_question_condition.csv`, `ref_rate_class.csv`) and
`_ground_truth.json`. Output is deterministic for a given seed.

Next step: `cd analytics && dbt build`.

### `extract`

```
s2r extract [--extractor lmstudio|mock] [--base-url <URL>] [--model <ID>] [--timeout-secs <N>]
```

| Option | Env | Default |
|---|---|---|
| `--extractor` | | `lmstudio` |
| `--base-url` | `LMSTUDIO_BASE_URL` | `http://127.0.0.1:1234/v1` |
| `--model` | `LMSTUDIO_MODEL` | `google/gemma-4-31b` |
| `--timeout-secs` | | `600` |

Reads Silver prescriptions, asks the model about the texts and drugs that the curated dictionary
cannot answer, validates the answers against allow-lists, and writes
`<data-dir>/llm_candidates.json`:

```json
{
  "model": "google/gemma-4-31b",
  "mappings":    [ { "text": "ELIQUIS 5MG", "drug": "apixaban", "confidence": 0.96 } ],
  "indications": [ { "drug": "apixaban", "condition": "AtrialFibrillation", "confidence": 0.94 } ],
  "rejected":    [ "indication 'apixaban' -> unknown condition 'Vampirism'" ]
}
```

`--extractor mock` uses a deterministic offline stand-in (used by CI).

LM Studio request: `POST {base-url}/chat/completions`, OpenAI-compatible, `temperature: 0`,
`response_format: {"type": "json_schema", ...}`. The answer is parsed as data only.

### `ask`

```
s2r ask [q1|q2|q3|q4|all] [--trust curated|llm] [--min-confidence <0..1>] [--quarter YYYY-Qn] [--show <N>]
```

| Option | Default | Meaning |
|---|---|---|
| question | `all` | which question(s) |
| `--trust` | `llm` | `curated`: dictionary only. `llm`: also LLM candidates ≥ `--min-confidence` (requires `extract`) |
| `--min-confidence` | `0.8` | threshold for LLM candidates |
| `--quarter` | `2026-Q3` | period for Q1 |
| `--show` | `4` | sample ids per row |

Q1 runs `mf query --metrics submissions,placement_rate,avg_face_amount --group-by
submission__channel,submission__product_type` with a fixed `--where` on `metric_time`, and
compares each row with a SPARQL aggregate. Q2–Q4 compare the Gold SQL in `app/queries/gold/`
with the reasoner's `uw:hasFinding` triples. If `_ground_truth.json` exists, each row is broken
down by planted scenario.

### `explain`

```
s2r explain <SUBMISSION_ID> [--trust curated|llm] [--min-confidence <0..1>]
```

Prints every `uw:hasFinding` on the submission as a derivation tree. Inner nodes show the rule
(business rules with their description); leaves show the origin of each asserted fact
(`silver.<table>`, `ontology`, `curated dictionary`, `LLM candidate (model, confidence)`).
Trees that rely on an LLM candidate are flagged.

### `demo`

```
s2r demo [--quarter YYYY-Qn]
```

`ask all` with `--trust llm` (falls back to `curated` if no candidates exist), then explains the
re-illustration finding on one case that Gold missed, preferring one that depends on LLM
knowledge.

### `export`

```
s2r export [--trust curated|llm] [--out <FILE>]     # default <data-dir>/graph.nt
```

Writes the reasoned graph (asserted + inferred triples) as sorted N-Triples, for inspection in
any RDF tool.

## Vocabulary

| Prefix | Namespace |
|---|---|
| `uw:` | `https://example.org/s2r/uw#` |
| `cond:` | `https://example.org/s2r/condition/` |
| `drug:` | `https://example.org/s2r/drug/` |
| `rc:` | `https://example.org/s2r/rate-class/` |
| `ex:` | `https://example.org/s2r/` (resources: `ex:submission/S000123`, `ex:party/P000045`, …) |

Findings: `uw:InsurableInterestNotEstablished` (Q2), `uw:PotentialNonDisclosure` (Q3),
`uw:PreferredIneligible`, `uw:TobaccoUseDetected`, `uw:RequiresReillustration` (Q4).

## MetricFlow metrics (`analytics/models/gold/_semantic.yml`)

| Metric | Type | Definition |
|---|---|---|
| `submissions` | simple | count of `submission_id` |
| `placed_submissions` | simple | sum of `is_placed` |
| `placement_rate` | ratio | `placed_submissions / submissions` |
| `avg_face_amount` | simple | average of `face_amount` (USD) |

Dimensions: `submission__channel`, `submission__product_type`, `submission__status`,
`metric_time` (from `received_date`).
