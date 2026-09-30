#!/usr/bin/env bash
# Scheduling only: trading conditions and stage transitions belong to the saved Flow.
set -euo pipefail
if [[ $# -lt 2 ]]; then
  printf 'Usage: bash scripts/run-cycle.sh FLOW.http.yml RUN_ID [--config PATH] [--execute] [--input NAME=JSON]\n' >&2
  exit 2
fi
cycle_file=$1
cycle_run_id=$2
shift 2
cycle_binary=${FLOW_BNB_BIN:-}
if [[ -z $cycle_binary ]]; then
  cycle_script_dir=$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
  if [[ -x "$cycle_script_dir/flow-bnb" ]]; then
    cycle_binary="$cycle_script_dir/flow-bnb"
  else
    cycle_binary=flow-bnb
  fi
fi
cycle_interval=${FLOW_BNB_POLL_SECONDS:-10}
[[ $cycle_interval =~ ^[1-9][0-9]*$ ]] || { printf 'FLOW_BNB_POLL_SECONDS must be a positive integer\n' >&2; exit 2; }
cycle_args=(cycle-step "$cycle_file" --run-id "$cycle_run_id" "$@")
cycle_status_args=(cycle-status --run-id "$cycle_run_id" --phase-only)
while [[ $# -gt 0 ]]; do
  case "$1" in
    --config)
      [[ $# -ge 2 ]] || { printf 'Missing --config value\n' >&2; exit 2; }
      cycle_status_args+=(--config "$2")
      shift 2 ;;
    --config=*) cycle_status_args+=("$1"); shift ;;
    *) shift ;;
  esac
done
while :; do
  "$cycle_binary" "${cycle_args[@]}"
  cycle_phase=$("$cycle_binary" "${cycle_status_args[@]}")
  case "$cycle_phase" in
    completed|completed_with_discrepancy) exit 0 ;;
    needs_attention|stopped) exit 1 ;;
  esac
  sleep "$cycle_interval"
done
