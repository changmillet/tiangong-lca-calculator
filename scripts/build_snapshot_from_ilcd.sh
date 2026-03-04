#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

if [ -f .env ]; then
  set -a
  # shellcheck source=/dev/null
  source .env
  set +a
fi

DB_URL="${DATABASE_URL:-${CONN:-}}"
if [ -z "$DB_URL" ]; then
  echo "missing DB connection: set DATABASE_URL or CONN" >&2
  exit 1
fi

PROCESS_STATES="${PROCESS_STATES:-100}"
PROCESS_LIMIT="${PROCESS_LIMIT:-0}"
PROVIDER_RULE="${PROVIDER_RULE:-strict_unique_provider}"
SCOPE="${SCOPE:-full_library}"
SELF_LOOP_CUTOFF="${SELF_LOOP_CUTOFF:-0.999999}"
SINGULAR_EPS="${SINGULAR_EPS:-1e-12}"
REPORT_DIR="${REPORT_DIR:-reports/snapshot-coverage}"
SNAPSHOT_ID="${SNAPSHOT_ID:-}"
METHOD_ID="${METHOD_ID:-}"
METHOD_VERSION="${METHOD_VERSION:-}"
NO_LCIA=0
ALL_STATES=0
PROCESS_STATES_LABEL=""

usage() {
  cat <<'USAGE'
Build one computable LCA snapshot from ILCD-like source tables (`processes`/`flows`/`lciamethods`).

Usage:
  scripts/build_snapshot_from_ilcd.sh [options]

Options:
  --snapshot-id <uuid>         explicit snapshot id (default: auto generated)
  --process-states <csv|all>   process state_code filter, use "all" to disable filter (default: "100")
  --process-limit <n>          limit processes for debug snapshot, 0 = no limit (default: 0)
  --provider-rule <rule>       provider matching rule (default: strict_unique_provider)
  --self-loop-cutoff <float>   drop technosphere diagonal edges with |value| >= cutoff (default: 0.999999)
  --singular-eps <float>       epsilon for near-singular diagonal checks (default: 1e-12)
  --report-dir <path>          write coverage report json/md files (default: reports/snapshot-coverage)
  --method-id <uuid>           LCIA method id (default: auto pick largest factor-count method)
  --method-version <version>   LCIA method version (required if --method-id is set)
  --no-lcia                    skip C matrix build (impact stage disabled)
  -h, --help                   show this help
USAGE
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --snapshot-id)
      SNAPSHOT_ID="$2"
      shift 2
      ;;
    --process-states)
      PROCESS_STATES="$2"
      shift 2
      ;;
    --process-limit)
      PROCESS_LIMIT="$2"
      shift 2
      ;;
    --provider-rule)
      PROVIDER_RULE="$2"
      shift 2
      ;;
    --self-loop-cutoff)
      SELF_LOOP_CUTOFF="$2"
      shift 2
      ;;
    --singular-eps)
      SINGULAR_EPS="$2"
      shift 2
      ;;
    --report-dir)
      REPORT_DIR="$2"
      shift 2
      ;;
    --method-id)
      METHOD_ID="$2"
      shift 2
      ;;
    --method-version)
      METHOD_VERSION="$2"
      shift 2
      ;;
    --no-lcia)
      NO_LCIA=1
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "unknown argument: $1" >&2
      usage
      exit 2
      ;;
  esac
done

PROCESS_STATES="$(echo "$PROCESS_STATES" | tr -d ' ')"
PROCESS_STATES_LABEL="$PROCESS_STATES"

if [ -z "$PROCESS_STATES" ] || [ "$PROCESS_STATES" = "all" ]; then
  ALL_STATES=1
  # Keep SQL casts valid while bypassing filter with all_states flag.
  PROCESS_STATES="0"
  PROCESS_STATES_LABEL="all"
fi

if [ -z "$SNAPSHOT_ID" ]; then
  SNAPSHOT_ID="$(cat /proc/sys/kernel/random/uuid)"
fi

if ! [[ "$PROCESS_LIMIT" =~ ^[0-9]+$ ]]; then
  echo "PROCESS_LIMIT must be a non-negative integer: $PROCESS_LIMIT" >&2
  exit 2
fi

if [ "$NO_LCIA" -eq 1 ]; then
  METHOD_ID="00000000-0000-0000-0000-000000000000"
  METHOD_VERSION="00.00.000"
  HAS_LCIA=0
else
  if [ -n "$METHOD_ID" ] && [ -z "$METHOD_VERSION" ]; then
    echo "--method-version is required when --method-id is set" >&2
    exit 2
  fi
  if [ -z "$METHOD_ID" ]; then
    IFS='|' read -r METHOD_ID METHOD_VERSION METHOD_FACTOR_CNT < <(
      psql "$DB_URL" -Atq -v ON_ERROR_STOP=1 -c "
      WITH m AS (
        SELECT
          id::text,
          version::text,
          CASE
            WHEN jsonb_typeof(json#>'{LCIAMethodDataSet,characterisationFactors,factor}') = 'array'
            THEN jsonb_array_length(json#>'{LCIAMethodDataSet,characterisationFactors,factor}')
            ELSE 0
          END AS factor_cnt
        FROM public.lciamethods
      )
      SELECT id || '|' || version || '|' || factor_cnt
      FROM m
      ORDER BY factor_cnt DESC, id
      LIMIT 1;
      "
    )
    if [ -z "${METHOD_ID:-}" ]; then
      echo "no lciamethods found; rerun with --no-lcia or load method data first" >&2
      exit 1
    fi
    echo "[info] auto-selected LCIA method id=$METHOD_ID version=$METHOD_VERSION factors=$METHOD_FACTOR_CNT"
  fi
  HAS_LCIA=1
fi

echo "[info] building snapshot"
echo "  snapshot_id   = $SNAPSHOT_ID"
echo "  process_states= $PROCESS_STATES_LABEL"
echo "  process_limit = $PROCESS_LIMIT"
echo "  provider_rule = $PROVIDER_RULE"
echo "  self_loop_cutoff = $SELF_LOOP_CUTOFF"
echo "  singular_eps  = $SINGULAR_EPS"
echo "  report_dir    = $REPORT_DIR"
if [ "$HAS_LCIA" -eq 1 ]; then
  echo "  method_id     = $METHOD_ID"
  echo "  method_version= $METHOD_VERSION"
else
  echo "  method        = disabled (--no-lcia)"
fi

BUILD_OUTPUT="$(
psql "$DB_URL" -Atq -v ON_ERROR_STOP=1 \
  -v snapshot_id="$SNAPSHOT_ID" \
  -v process_states="$PROCESS_STATES" \
  -v all_states="$ALL_STATES" \
  -v process_limit="$PROCESS_LIMIT" \
  -v provider_rule="$PROVIDER_RULE" \
  -v self_loop_cutoff="$SELF_LOOP_CUTOFF" \
  -v singular_eps="$SINGULAR_EPS" \
  -v scope="$SCOPE" \
  -v has_lcia="$HAS_LCIA" \
  -v method_id="$METHOD_ID" \
  -v method_version="$METHOD_VERSION" \
<<'SQL'
BEGIN;
SET LOCAL statement_timeout = 0;

INSERT INTO public.lca_network_snapshots (
  id,
  scope,
  provider_matching_rule,
  lcia_method_id,
  lcia_method_version,
  status,
  process_filter,
  created_at,
  updated_at
)
VALUES (
  :'snapshot_id'::uuid,
  :'scope',
  :'provider_rule',
  CASE WHEN :'has_lcia'::int = 1 THEN :'method_id'::uuid ELSE NULL END,
  CASE WHEN :'has_lcia'::int = 1 THEN :'method_version'::bpchar ELSE NULL END,
  'draft',
  CASE
    WHEN :'all_states'::int = 1 THEN jsonb_build_object('all_states', true)
    ELSE jsonb_build_object(
      'all_states',
      false,
      'process_states',
      string_to_array(:'process_states', ',')::int[]
    )
  END,
  NOW(),
  NOW()
);

WITH candidate_processes AS (
  SELECT p.id, p.version, p.state_code
  FROM public.processes p
  WHERE
    (
      :'all_states'::int = 1
      OR p.state_code = ANY (string_to_array(:'process_states', ',')::int[])
    )
    AND p.json ? 'processDataSet'
  ORDER BY p.id, p.version
  LIMIT NULLIF(:'process_limit'::int, 0)
),
ins AS (
  INSERT INTO public.lca_process_index (
    snapshot_id, process_idx, process_id, process_version, state_code
  )
  SELECT
    :'snapshot_id'::uuid,
    row_number() OVER (ORDER BY cp.id, cp.version) - 1,
    cp.id,
    cp.version,
    cp.state_code
  FROM candidate_processes cp
  RETURNING 1
)
SELECT COUNT(*) AS inserted_process_index_rows FROM ins;

CREATE TEMP TABLE tmp_processes AS
SELECT
  pi.process_id,
  pi.process_version,
  p.json
FROM public.lca_process_index pi
JOIN public.processes p
  ON p.id = pi.process_id
 AND p.version = pi.process_version
WHERE pi.snapshot_id = :'snapshot_id'::uuid;

CREATE TEMP TABLE tmp_exchanges AS
WITH ex_arr AS (
  SELECT
    tp.process_id,
    tp.process_version,
    CASE
      WHEN jsonb_typeof(tp.json#>'{processDataSet,exchanges,exchange}') = 'array'
      THEN tp.json#>'{processDataSet,exchanges,exchange}'
      WHEN jsonb_typeof(tp.json#>'{processDataSet,exchanges,exchange}') = 'object'
      THEN jsonb_build_array(tp.json#>'{processDataSet,exchanges,exchange}')
      ELSE '[]'::jsonb
    END AS arr
  FROM tmp_processes tp
),
raw AS (
  SELECT process_id, process_version, jsonb_array_elements(arr) AS item
  FROM ex_arr
),
norm AS (
  SELECT
    process_id,
    process_version,
    item->>'exchangeDirection' AS exchange_direction,
    NULLIF(regexp_replace(COALESCE(item->>'meanAmount', item->>'resultingAmount', ''), ',', '', 'g'), '') AS amount_text,
    NULLIF(item#>>'{referenceToFlowDataSet,@refObjectId}', '') AS flow_id_text
  FROM raw
)
SELECT
  process_id,
  process_version,
  exchange_direction,
  CASE
    WHEN amount_text ~ '^[+-]?((\d+\.?\d*)|(\.\d+))([eE][+-]?\d+)?$'
    THEN amount_text::double precision
    ELSE NULL
  END AS amount,
  CASE
    WHEN flow_id_text ~* '^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$'
    THEN flow_id_text::uuid
    ELSE NULL
  END AS flow_id
FROM norm
WHERE flow_id_text IS NOT NULL;

CREATE TEMP TABLE tmp_output_providers AS
SELECT DISTINCT process_id, flow_id
FROM tmp_exchanges
WHERE exchange_direction = 'Output'
  AND flow_id IS NOT NULL;

CREATE INDEX tmp_output_providers_flow_process_idx
  ON tmp_output_providers (flow_id, process_id);

CREATE TEMP TABLE tmp_input_exchanges AS
SELECT process_id, flow_id, amount
FROM tmp_exchanges
WHERE exchange_direction = 'Input'
  AND flow_id IS NOT NULL
  AND amount IS NOT NULL;

CREATE INDEX tmp_exchanges_process_flow_dir_idx
  ON tmp_exchanges (process_id, flow_id, exchange_direction);
CREATE INDEX tmp_exchanges_flow_dir_idx
  ON tmp_exchanges (flow_id, exchange_direction);
CREATE INDEX tmp_input_exchanges_flow_process_idx
  ON tmp_input_exchanges (flow_id, process_id);

CREATE TEMP TABLE tmp_provider_counts AS
SELECT flow_id, COUNT(DISTINCT process_id)::int AS provider_cnt
FROM tmp_output_providers
GROUP BY flow_id;

CREATE TEMP TABLE tmp_lcia_factors (
  flow_id uuid NOT NULL,
  cf_value double precision NOT NULL
) ON COMMIT DROP;

INSERT INTO tmp_lcia_factors (flow_id, cf_value)
WITH method_factors AS (
  SELECT
    CASE
      WHEN jsonb_typeof(m.json#>'{LCIAMethodDataSet,characterisationFactors,factor}') = 'array'
      THEN m.json#>'{LCIAMethodDataSet,characterisationFactors,factor}'
      ELSE '[]'::jsonb
    END AS factors
  FROM public.lciamethods m
  WHERE :'has_lcia'::int = 1
    AND m.id = :'method_id'::uuid
    AND m.version = :'method_version'::bpchar
),
raw AS (
  SELECT jsonb_array_elements(factors) AS item
  FROM method_factors
),
norm AS (
  SELECT
    NULLIF(item#>>'{referenceToFlowDataSet,@refObjectId}', '') AS flow_id_text,
    NULLIF(regexp_replace(COALESCE(item->>'meanValue', ''), ',', '', 'g'), '') AS cf_value_text
  FROM raw
)
SELECT
  flow_id_text::uuid AS flow_id,
  cf_value_text::double precision AS cf_value
FROM norm
WHERE flow_id_text ~* '^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$'
  AND cf_value_text ~ '^[+-]?((\d+\.?\d*)|(\.\d+))([eE][+-]?\d+)?$';

WITH flow_candidates AS (
  SELECT DISTINCT flow_id
  FROM tmp_exchanges
  WHERE flow_id IS NOT NULL
  UNION
  SELECT DISTINCT flow_id
  FROM tmp_lcia_factors
),
flow_meta AS (
  SELECT
    fc.flow_id,
    fm.version AS flow_version,
    fm.json AS flow_json
  FROM flow_candidates fc
  LEFT JOIN LATERAL (
    SELECT f.version, f.json, f.state_code, f.modified_at, f.created_at
    FROM public.flows f
    WHERE f.id = fc.flow_id
    ORDER BY f.state_code DESC, f.modified_at DESC NULLS LAST, f.created_at DESC NULLS LAST
    LIMIT 1
  ) fm ON TRUE
),
ins AS (
  INSERT INTO public.lca_flow_index (
    snapshot_id, flow_idx, flow_id, flow_version, flow_kind
  )
  SELECT
    :'snapshot_id'::uuid,
    row_number() OVER (ORDER BY fm.flow_id) - 1,
    fm.flow_id,
    fm.flow_version,
    CASE
      WHEN COALESCE(
        fm.flow_json#>>'{flowDataSet,flowInformation,dataSetInformation,classificationInformation,common:elementaryFlowCategorization,common:category,0,#text}',
        fm.flow_json#>>'{flowDataSet,flowInformation,dataSetInformation,classificationInformation,common:elementaryFlowCategorization,common:category,#text}'
      ) IN ('Emissions', 'Resources', 'Land use')
      THEN 'elementary'
      ELSE 'product'
    END
  FROM flow_meta fm
  RETURNING 1
)
SELECT COUNT(*) AS inserted_flow_index_rows FROM ins;

CREATE TEMP TABLE tmp_unique_provider AS
SELECT DISTINCT ON (op.flow_id)
  op.flow_id,
  op.process_id AS provider_process_id
FROM tmp_output_providers op
JOIN tmp_provider_counts pc
  ON pc.flow_id = op.flow_id
 AND pc.provider_cnt = 1
ORDER BY op.flow_id, op.process_id;

CREATE TEMP TABLE tmp_technosphere_agg AS
SELECT
  cpi.process_idx AS row,
  ppi.process_idx AS col,
  SUM(ie.amount) AS value
FROM tmp_input_exchanges ie
JOIN tmp_unique_provider up
  ON up.flow_id = ie.flow_id
JOIN public.lca_process_index cpi
  ON cpi.snapshot_id = :'snapshot_id'::uuid
 AND cpi.process_id = ie.process_id
JOIN public.lca_process_index ppi
  ON ppi.snapshot_id = :'snapshot_id'::uuid
 AND ppi.process_id = up.provider_process_id
GROUP BY cpi.process_idx, ppi.process_idx
HAVING SUM(ie.amount) <> 0;

WITH filtered AS (
  SELECT row, col, value
  FROM tmp_technosphere_agg
  WHERE NOT (row = col AND ABS(value) >= :'self_loop_cutoff'::double precision)
),
ins AS (
  INSERT INTO public.lca_technosphere_entries (
    snapshot_id, row, col, value, input_process_idx, provider_process_idx, flow_id
  )
  SELECT
    :'snapshot_id'::uuid,
    f.row,
    f.col,
    f.value,
    f.row,
    f.col,
    NULL::uuid
  FROM filtered f
  RETURNING 1
)
SELECT COUNT(*) AS inserted_technosphere_rows FROM ins;

WITH edges AS (
  SELECT
    fi.flow_idx AS row,
    pi.process_idx AS col,
    CASE
      WHEN e.exchange_direction = 'Input' THEN -e.amount
      ELSE e.amount
    END AS value,
    fi.flow_id
  FROM tmp_exchanges e
  JOIN public.lca_process_index pi
    ON pi.snapshot_id = :'snapshot_id'::uuid
   AND pi.process_id = e.process_id
  JOIN public.lca_flow_index fi
    ON fi.snapshot_id = :'snapshot_id'::uuid
   AND fi.flow_id = e.flow_id
  WHERE fi.flow_kind = 'elementary'
    AND e.amount IS NOT NULL
    AND e.exchange_direction IN ('Input', 'Output')
),
agg AS (
  SELECT row, col, SUM(value) AS value, MIN(flow_id::text)::uuid AS flow_id
  FROM edges
  GROUP BY row, col
  HAVING SUM(value) <> 0
),
ins AS (
  INSERT INTO public.lca_biosphere_entries (
    snapshot_id, row, col, value, flow_id, process_idx
  )
  SELECT
    :'snapshot_id'::uuid,
    a.row,
    a.col,
    a.value,
    a.flow_id,
    a.col
  FROM agg a
  RETURNING 1
)
SELECT COUNT(*) AS inserted_biosphere_rows FROM ins;

WITH edges AS (
  SELECT
    fi.flow_idx AS col,
    SUM(cf.cf_value) AS value
  FROM tmp_lcia_factors cf
  JOIN public.lca_flow_index fi
    ON fi.snapshot_id = :'snapshot_id'::uuid
   AND fi.flow_id = cf.flow_id
  GROUP BY fi.flow_idx
  HAVING SUM(cf.cf_value) <> 0
),
ins AS (
  INSERT INTO public.lca_characterization_factors (
    snapshot_id, row, col, value, method_id, method_version, indicator_key
  )
  SELECT
    :'snapshot_id'::uuid,
    0,
    e.col,
    e.value,
    CASE WHEN :'has_lcia'::int = 1 THEN :'method_id'::uuid ELSE NULL END,
    CASE WHEN :'has_lcia'::int = 1 THEN :'method_version'::bpchar ELSE NULL END,
    CASE
      WHEN :'has_lcia'::int = 1 THEN :'method_id' || ':' || :'method_version'
      ELSE 'no_lcia'
    END
  FROM edges e
  RETURNING 1
)
SELECT COUNT(*) AS inserted_characterization_rows FROM ins;

WITH metrics AS (
  SELECT
    (SELECT COUNT(*) FROM public.lca_process_index WHERE snapshot_id = :'snapshot_id'::uuid) AS process_count,
    (SELECT COUNT(*) FROM public.lca_flow_index WHERE snapshot_id = :'snapshot_id'::uuid) AS flow_count,
    (SELECT COUNT(*) FROM public.lca_technosphere_entries WHERE snapshot_id = :'snapshot_id'::uuid) AS a_nnz,
    (SELECT COUNT(*) FROM public.lca_biosphere_entries WHERE snapshot_id = :'snapshot_id'::uuid) AS b_nnz,
    (SELECT COUNT(*) FROM public.lca_characterization_factors WHERE snapshot_id = :'snapshot_id'::uuid) AS c_nnz,
    (SELECT COUNT(*) FROM tmp_input_exchanges) AS input_edges_total,
    (SELECT COUNT(*) FROM tmp_input_exchanges ie JOIN tmp_provider_counts pc ON pc.flow_id = ie.flow_id WHERE pc.provider_cnt = 1) AS input_edges_matched_unique,
    (SELECT COUNT(*) FROM tmp_input_exchanges ie JOIN tmp_provider_counts pc ON pc.flow_id = ie.flow_id WHERE pc.provider_cnt > 1) AS input_edges_matched_multi
),
updated AS (
  UPDATE public.lca_network_snapshots s
  SET
    status = 'ready',
    source_hash = md5(concat_ws(
      ':',
      m.process_count,
      m.flow_count,
      m.a_nnz,
      m.b_nnz,
      m.c_nnz,
      :'provider_rule',
      CASE WHEN :'has_lcia'::int = 1 THEN :'method_id' || ':' || :'method_version' ELSE 'no_lcia' END
    )),
    updated_at = NOW()
  FROM metrics m
  WHERE s.id = :'snapshot_id'::uuid
  RETURNING s.id, s.status, s.source_hash
)
SELECT * FROM updated;

WITH base AS (
  SELECT
    (SELECT COUNT(*) FROM public.lca_process_index WHERE snapshot_id = :'snapshot_id'::uuid) AS process_count,
    (SELECT COUNT(*) FROM public.lca_flow_index WHERE snapshot_id = :'snapshot_id'::uuid) AS flow_count,
    (SELECT COALESCE(MAX("row"), -1) + 1 FROM public.lca_characterization_factors WHERE snapshot_id = :'snapshot_id'::uuid) AS impact_count,
    (SELECT COUNT(*) FROM public.lca_technosphere_entries WHERE snapshot_id = :'snapshot_id'::uuid) AS a_nnz,
    (SELECT COUNT(*) FROM public.lca_biosphere_entries WHERE snapshot_id = :'snapshot_id'::uuid) AS b_nnz,
    (SELECT COUNT(*) FROM public.lca_characterization_factors WHERE snapshot_id = :'snapshot_id'::uuid) AS c_nnz,
    (SELECT COUNT(*) FROM tmp_input_exchanges) AS input_edges_total,
    (SELECT COUNT(*) FROM tmp_input_exchanges ie JOIN tmp_provider_counts pc ON pc.flow_id = ie.flow_id WHERE pc.provider_cnt = 1) AS input_edges_matched_unique,
    (SELECT COUNT(*) FROM tmp_input_exchanges ie JOIN tmp_provider_counts pc ON pc.flow_id = ie.flow_id WHERE pc.provider_cnt > 1) AS input_edges_matched_multi,
    (SELECT COUNT(*) FROM tmp_input_exchanges ie LEFT JOIN tmp_provider_counts pc ON pc.flow_id = ie.flow_id WHERE pc.flow_id IS NULL) AS input_edges_unmatched,
    (
      SELECT COUNT(*)
      FROM tmp_technosphere_agg
      WHERE row = col
        AND ABS(value) >= :'self_loop_cutoff'::double precision
    ) AS dropped_singular_self_loops,
    (
      SELECT COUNT(*)
      FROM public.lca_technosphere_entries
      WHERE snapshot_id = :'snapshot_id'::uuid
        AND "row" <> "col"
    ) AS a_offdiag_nnz
),
diag_a AS (
  SELECT "row" AS process_idx, SUM(value) AS a_diag
  FROM public.lca_technosphere_entries
  WHERE snapshot_id = :'snapshot_id'::uuid
    AND "row" = "col"
  GROUP BY "row"
),
diag_eval AS (
  SELECT
    pn.process_idx,
    COALESCE(da.a_diag, 0.0) AS a_diag,
    ABS(1.0 - COALESCE(da.a_diag, 0.0)) AS abs_m_diag
  FROM public.lca_process_index pn
  LEFT JOIN diag_a da
    ON da.process_idx = pn.process_idx
  WHERE pn.snapshot_id = :'snapshot_id'::uuid
),
diag_stats AS (
  SELECT
    COUNT(*) FILTER (WHERE ABS(a_diag) >= :'self_loop_cutoff'::double precision) AS a_diag_ge_cutoff,
    COUNT(*) FILTER (WHERE abs_m_diag <= :'singular_eps'::double precision) AS m_zero_diag_count,
    COALESCE(MIN(abs_m_diag), 0.0) AS m_min_abs_diag
  FROM diag_eval
),
report AS (
  SELECT
    b.process_count,
    b.flow_count,
    b.impact_count,
    b.a_nnz,
    b.b_nnz,
    b.c_nnz,
    b.input_edges_total,
    b.input_edges_matched_unique,
    b.input_edges_matched_multi,
    b.input_edges_unmatched,
    CASE
      WHEN b.input_edges_total = 0 THEN 0
      ELSE ROUND((b.input_edges_matched_unique::numeric / b.input_edges_total::numeric) * 100.0, 4)
    END AS unique_provider_match_pct,
    CASE
      WHEN b.input_edges_total = 0 THEN 0
      ELSE ROUND(((b.input_edges_matched_unique + b.input_edges_matched_multi)::numeric / b.input_edges_total::numeric) * 100.0, 4)
    END AS any_provider_match_pct,
    b.dropped_singular_self_loops,
    ds.a_diag_ge_cutoff,
    ds.m_zero_diag_count,
    ds.m_min_abs_diag,
    (b.a_offdiag_nnz + GREATEST(b.process_count - ds.m_zero_diag_count, 0)) AS m_nnz_estimated,
    CASE
      WHEN b.process_count = 0 THEN 1
      ELSE ROUND(
        1 - (
          (b.a_offdiag_nnz + GREATEST(b.process_count - ds.m_zero_diag_count, 0))::numeric
          / (b.process_count::numeric * b.process_count::numeric)
        ),
        8
      )
    END AS m_sparsity
  FROM base b
  CROSS JOIN diag_stats ds
)
SELECT
  concat_ws(
    '|',
    'COVERAGE_METRICS',
    process_count::text,
    flow_count::text,
    impact_count::text,
    a_nnz::text,
    b_nnz::text,
    c_nnz::text,
    input_edges_total::text,
    input_edges_matched_unique::text,
    input_edges_matched_multi::text,
    input_edges_unmatched::text,
    unique_provider_match_pct::text,
    any_provider_match_pct::text,
    dropped_singular_self_loops::text,
    a_diag_ge_cutoff::text,
    m_zero_diag_count::text,
    m_min_abs_diag::text,
    m_nnz_estimated::text,
    m_sparsity::text
  )
FROM report;

COMMIT;
SQL
)"

echo "$BUILD_OUTPUT"

mkdir -p "$REPORT_DIR"
REPORT_JSON_PATH="$REPORT_DIR/$SNAPSHOT_ID.json"
REPORT_MD_PATH="$REPORT_DIR/$SNAPSHOT_ID.md"

METRICS_LINE="$(echo "$BUILD_OUTPUT" | rg -F 'COVERAGE_METRICS|' -m1 || true)"
if [ -z "$METRICS_LINE" ]; then
  echo "failed to parse coverage metrics from build output" >&2
  exit 1
fi

IFS='|' read -r \
  _ \
  RPT_PROCESS_COUNT \
  RPT_FLOW_COUNT \
  RPT_IMPACT_COUNT \
  RPT_A_NNZ \
  RPT_B_NNZ \
  RPT_C_NNZ \
  RPT_INPUT_EDGES_TOTAL \
  RPT_MATCHED_UNIQUE \
  RPT_MATCHED_MULTI \
  RPT_UNMATCHED_NO_PROVIDER \
  RPT_UNIQUE_MATCH_PCT \
  RPT_ANY_MATCH_PCT \
  RPT_PREFILTER_DIAG_GE_CUTOFF \
  RPT_A_DIAG_GE_CUTOFF \
  RPT_M_ZERO_DIAG_COUNT \
  RPT_M_MIN_ABS_DIAG \
  RPT_M_NNZ_ESTIMATED \
  RPT_M_SPARSITY \
<<<"$METRICS_LINE"

RPT_SINGULAR_RISK_LEVEL="low"
if [ "${RPT_M_ZERO_DIAG_COUNT:-0}" -gt 0 ]; then
  RPT_SINGULAR_RISK_LEVEL="high"
elif [ "${RPT_PREFILTER_DIAG_GE_CUTOFF:-0}" -gt 0 ] || [ "${RPT_A_DIAG_GE_CUTOFF:-0}" -gt 0 ]; then
  RPT_SINGULAR_RISK_LEVEL="medium"
fi

cat > "$REPORT_JSON_PATH" <<JSON
{
  "snapshot_id": "$SNAPSHOT_ID",
  "generated_at_utc": "$(date -u +%Y-%m-%dT%H:%M:%SZ)",
  "config": {
    "process_states": "$PROCESS_STATES_LABEL",
    "process_limit": $PROCESS_LIMIT,
    "provider_rule": "$PROVIDER_RULE",
    "self_loop_cutoff": $SELF_LOOP_CUTOFF,
    "singular_eps": $SINGULAR_EPS,
    "has_lcia": $HAS_LCIA,
    "method_id": "$METHOD_ID",
    "method_version": "$METHOD_VERSION"
  },
  "coverage": {
    "matching": {
      "input_edges_total": $RPT_INPUT_EDGES_TOTAL,
      "matched_unique_provider": $RPT_MATCHED_UNIQUE,
      "matched_multi_provider": $RPT_MATCHED_MULTI,
      "unmatched_no_provider": $RPT_UNMATCHED_NO_PROVIDER,
      "unique_provider_match_pct": $RPT_UNIQUE_MATCH_PCT,
      "any_provider_match_pct": $RPT_ANY_MATCH_PCT
    },
    "singular_risk": {
      "risk_level": "$RPT_SINGULAR_RISK_LEVEL",
      "prefilter_diag_abs_ge_cutoff": $RPT_PREFILTER_DIAG_GE_CUTOFF,
      "postfilter_a_diag_abs_ge_cutoff": $RPT_A_DIAG_GE_CUTOFF,
      "m_zero_diagonal_count": $RPT_M_ZERO_DIAG_COUNT,
      "m_min_abs_diagonal": $RPT_M_MIN_ABS_DIAG
    },
    "matrix_scale": {
      "process_count": $RPT_PROCESS_COUNT,
      "flow_count": $RPT_FLOW_COUNT,
      "impact_count": $RPT_IMPACT_COUNT,
      "a_nnz": $RPT_A_NNZ,
      "b_nnz": $RPT_B_NNZ,
      "c_nnz": $RPT_C_NNZ,
      "m_nnz_estimated": $RPT_M_NNZ_ESTIMATED,
      "m_sparsity_estimated": $RPT_M_SPARSITY
    }
  }
}
JSON

cat > "$REPORT_MD_PATH" <<MD
# Snapshot Coverage Report

- snapshot_id: \`$SNAPSHOT_ID\`
- generated_at_utc: \`$(date -u +%Y-%m-%dT%H:%M:%SZ)\`
- process_states: \`$PROCESS_STATES_LABEL\`
- process_limit: \`$PROCESS_LIMIT\`
- provider_rule: \`$PROVIDER_RULE\`
- self_loop_cutoff: \`$SELF_LOOP_CUTOFF\`
- singular_eps: \`$SINGULAR_EPS\`
- has_lcia: \`$HAS_LCIA\`
- method: \`$METHOD_ID@$METHOD_VERSION\`

## Matching Coverage

- input_edges_total: \`$RPT_INPUT_EDGES_TOTAL\`
- matched_unique_provider: \`$RPT_MATCHED_UNIQUE\`
- matched_multi_provider: \`$RPT_MATCHED_MULTI\`
- unmatched_no_provider: \`$RPT_UNMATCHED_NO_PROVIDER\`
- unique_provider_match_pct: \`$RPT_UNIQUE_MATCH_PCT\`
- any_provider_match_pct: \`$RPT_ANY_MATCH_PCT\`

## Singular Risk

- risk_level: \`$RPT_SINGULAR_RISK_LEVEL\`
- prefilter_diag_abs_ge_cutoff: \`$RPT_PREFILTER_DIAG_GE_CUTOFF\`
- postfilter_a_diag_abs_ge_cutoff: \`$RPT_A_DIAG_GE_CUTOFF\`
- m_zero_diagonal_count: \`$RPT_M_ZERO_DIAG_COUNT\`
- m_min_abs_diagonal: \`$RPT_M_MIN_ABS_DIAG\`

## Matrix Scale

- process_count (n): \`$RPT_PROCESS_COUNT\`
- flow_count: \`$RPT_FLOW_COUNT\`
- impact_count: \`$RPT_IMPACT_COUNT\`
- a_nnz: \`$RPT_A_NNZ\`
- b_nnz: \`$RPT_B_NNZ\`
- c_nnz: \`$RPT_C_NNZ\`
- m_nnz_estimated: \`$RPT_M_NNZ_ESTIMATED\`
- m_sparsity_estimated: \`$RPT_M_SPARSITY\`
MD

echo
echo "[done] snapshot built: $SNAPSHOT_ID"
echo "[report] coverage json: $REPORT_JSON_PATH"
echo "[report] coverage md:   $REPORT_MD_PATH"
echo "[summary] matching unique=$RPT_UNIQUE_MATCH_PCT% any=$RPT_ANY_MATCH_PCT% singular_risk=$RPT_SINGULAR_RISK_LEVEL"
echo "[next] run end-to-end debug:"
echo "  SNAPSHOT_ID=$SNAPSHOT_ID ./scripts/run_full_compute_debug.sh --snapshot-id $SNAPSHOT_ID"
