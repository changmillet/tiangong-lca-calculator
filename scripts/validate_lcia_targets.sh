#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Validate one-impact LCIA values against expected target values.

Required env:
  USER_API_KEY           Base64 api key used by Edge auth

Required args:
  --snapshot-id <uuid>   Snapshot id used by query
  --expected <path>      TSV file: process_id<TAB>expected_value[<TAB>abs_tol]
  --base-url <url>       Explicit HTTP(S) Edge base URL; alternatively set
                        LCIA_VALIDATION_BASE_URL (the flag takes precedence)

Optional args:
  --impact-id <uuid>     Impact method id to compare (default: GWP/Climate change)
  --default-abs-tol <n>  Default absolute tolerance (default: 1e-9)
  --out <path>           Write comparison TSV output
  -h, --help             Show help

Example:
  USER_API_KEY=... ./scripts/validate_lcia_targets.sh \
    --base-url http://127.0.0.1:54321/functions/v1 \
    --snapshot-id 9319be83-be17-40ca-a8db-d829f8e93d9e \
    --impact-id 6209b35f-9447-40b5-b68c-a1099e3674a0 \
    --expected reports/bw25-validation/user10-impact-target.tsv \
    --out reports/bw25-validation/user10-impact-compare.tsv
USAGE
}

BASE_URL="${LCIA_VALIDATION_BASE_URL:-}"
# Default LCIA impact category: Climate change (GWP)
DEFAULT_GWP_IMPACT_ID="${DEFAULT_GWP_IMPACT_ID:-6209b35f-9447-40b5-b68c-a1099e3674a0}"
SNAPSHOT_ID=""
IMPACT_ID="$DEFAULT_GWP_IMPACT_ID"
EXPECTED_FILE=""
DEFAULT_ABS_TOL="1e-9"
OUT_FILE=""

while [ "$#" -gt 0 ]; do
  case "$1" in
    --snapshot-id)
      SNAPSHOT_ID="$2"
      shift 2
      ;;
    --impact-id)
      IMPACT_ID="$2"
      shift 2
      ;;
    --expected)
      EXPECTED_FILE="$2"
      shift 2
      ;;
    --base-url)
      if [ "$#" -lt 2 ] || [[ "$2" == --* ]]; then
        echo "missing value for --base-url" >&2
        exit 2
      fi
      BASE_URL="$2"
      shift 2
      ;;
    --default-abs-tol)
      DEFAULT_ABS_TOL="$2"
      shift 2
      ;;
    --out)
      OUT_FILE="$2"
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "unknown arg: $1" >&2
      usage
      exit 2
      ;;
  esac
done

if [ -z "$BASE_URL" ]; then
  echo "missing --base-url or LCIA_VALIDATION_BASE_URL; no default remote target is selected" >&2
  exit 2
fi

# A destination is operator-selected. Reject malformed targets and embedded
# credentials before sending the separately supplied authorization header.
if [[ ! "$BASE_URL" =~ ^https?://[^/?#@[:space:]]+(/[^?#[:space:]]*)?$ ]]; then
  echo "invalid Edge base URL: require HTTP(S), a host, and no userinfo, query, fragment or whitespace" >&2
  exit 2
fi
while [[ "$BASE_URL" == */ ]]; do
  BASE_URL="${BASE_URL%/}"
done

if ! command -v jq >/dev/null 2>&1; then
  echo "missing required command: jq" >&2
  exit 1
fi

if [ -z "${USER_API_KEY:-}" ]; then
  echo "missing USER_API_KEY env" >&2
  exit 2
fi

if [ -z "$SNAPSHOT_ID" ] || [ -z "$EXPECTED_FILE" ]; then
  echo "missing required args" >&2
  usage
  exit 2
fi

if [ ! -f "$EXPECTED_FILE" ]; then
  echo "expected file not found: $EXPECTED_FILE" >&2
  exit 2
fi

TMP_EXPECTED="$(mktemp)"
TMP_RESP="$(mktemp)"
TMP_VALUES="$(mktemp)"
trap 'rm -f "$TMP_EXPECTED" "$TMP_RESP" "$TMP_VALUES"' EXIT

# Keep first three columns only: process_id, expected_value, abs_tol(optional).
awk -F $'\t' '
  NF >= 2 {
    pid=$1; expected_val=$2; tol=(NF>=3?$3:"");
    gsub(/^[[:space:]]+|[[:space:]]+$/, "", pid);
    gsub(/^[[:space:]]+|[[:space:]]+$/, "", expected_val);
    gsub(/^[[:space:]]+|[[:space:]]+$/, "", tol);
    if (pid != "" && expected_val ~ /^[-+]?[0-9]*([.][0-9]+)?([eE][-+]?[0-9]+)?$/) {
      print pid "\t" expected_val "\t" tol;
    }
  }
' "$EXPECTED_FILE" > "$TMP_EXPECTED"

if [ ! -s "$TMP_EXPECTED" ]; then
  echo "expected file has no valid rows: $EXPECTED_FILE" >&2
  exit 2
fi

PROCESS_IDS_JSON="$(
  awk -F $'\t' '{print $1}' "$TMP_EXPECTED" \
    | jq -R . \
    | jq -s .
)"

REQUEST_BODY="$(
  jq -n \
    --arg scope "prod" \
    --arg snapshot_id "$SNAPSHOT_ID" \
    --arg impact_id "$IMPACT_ID" \
    --argjson process_ids "$PROCESS_IDS_JSON" \
    '{
      scope: $scope,
      snapshot_id: $snapshot_id,
      mode: "processes_one_impact",
      impact_id: $impact_id,
      process_ids: $process_ids
    }'
)"

echo "using impact_id: $IMPACT_ID" >&2

curl -sS -X POST "$BASE_URL/lca_query_results" \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer $USER_API_KEY" \
  --data "$REQUEST_BODY" > "$TMP_RESP"

if ! jq -e '.data.values and (.data.values|type=="object")' "$TMP_RESP" >/dev/null 2>&1; then
  echo "query failed:" >&2
  jq . "$TMP_RESP" >&2
  exit 1
fi

jq -r '.data.values | to_entries[] | [.key, (.value|tostring)] | @tsv' "$TMP_RESP" > "$TMP_VALUES"

HEADER=$'process_id\texpected\tactual\tabs_diff\tabs_tol\tpass'
if [ -n "$OUT_FILE" ]; then
  mkdir -p "$(dirname "$OUT_FILE")"
  printf '%s\n' "$HEADER" > "$OUT_FILE"
else
  printf '%s\n' "$HEADER"
fi

FAIL_COUNT=0
while IFS=$'\t' read -r PID EXPECTED TOL; do
  ACTUAL="$(awk -F $'\t' -v pid="$PID" '$1==pid{print $2; exit}' "$TMP_VALUES")"
  if [ -z "$ACTUAL" ]; then
    ACTUAL="NaN"
  fi

  USE_TOL="$DEFAULT_ABS_TOL"
  if [ -n "$TOL" ]; then
    USE_TOL="$TOL"
  fi

  # awk handles numeric diff robustly and returns pass/fail.
  ROW="$(
    awk -v pid="$PID" -v e="$EXPECTED" -v a="$ACTUAL" -v t="$USE_TOL" '
      BEGIN {
        diff = e - a;
        if (diff < 0) diff = -diff;
        pass = (diff <= t) ? "true" : "false";
        printf "%s\t%s\t%s\t%.17g\t%.17g\t%s", pid, e, a, diff, t, pass;
      }
    '
  )"

  PASS="$(printf '%s' "$ROW" | awk -F $'\t' '{print $6}')"
  if [ "$PASS" != "true" ]; then
    FAIL_COUNT=$((FAIL_COUNT + 1))
  fi

  if [ -n "$OUT_FILE" ]; then
    printf '%s\n' "$ROW" >> "$OUT_FILE"
  else
    printf '%s\n' "$ROW"
  fi
done < "$TMP_EXPECTED"

if [ -n "$OUT_FILE" ]; then
  echo "comparison written: $OUT_FILE"
fi

if [ "$FAIL_COUNT" -gt 0 ]; then
  echo "validation failed: $FAIL_COUNT row(s) out of tolerance" >&2
  exit 3
fi

echo "validation passed: all rows within tolerance"
