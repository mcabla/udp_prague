#!/usr/bin/env bash
set -euo pipefail
export LC_ALL=C

SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}" )" && pwd)
MEASURE_SCRIPT="$SCRIPT_DIR/measure_release_localhost_perf.sh"

MODE=${1:-classic}
DURATION=${2:-12}
RUNS_PER_BATCH=${3:-10}
BATCHES=${4:-3}
WARMUP=${WARMUP:-3}
BATCH_PAUSE=${BATCH_PAUSE:-20}
BASE_PORT=${BASE_PORT:-39410}

if [[ ! -f "$MEASURE_SCRIPT" ]]; then
    echo "Measurement script not found: $MEASURE_SCRIPT" >&2
    exit 1
fi

tmpdir=$(mktemp -d)
cleanup() {
    rm -rf "$tmpdir"
}
trap cleanup EXIT

all_data="$tmpdir/all_data.log"

echo "== Batched fair performance alignment =="
echo "mode=$MODE duration=${DURATION}s runs_per_batch=$RUNS_PER_BATCH batches=$BATCHES warmup=${WARMUP}s batch_pause=${BATCH_PAUSE}s"
echo "This wrapper runs the fair quiet-mode harness multiple times and aggregates all per-run results."

batch=1
while (( batch <= BATCHES )); do
    echo
    echo "######## Batch $batch/$BATCHES ########"
    batch_log="$tmpdir/batch_${batch}.log"

    SKIP_BUILD=0
    if (( batch > 1 )); then
        SKIP_BUILD=1
    fi

    WARMUP="$WARMUP" \
    SKIP_BUILD="$SKIP_BUILD" \
    EMIT_PARSE_LINES=1 \
    BASE_PORT="$((BASE_PORT + (batch - 1) * 10))" \
    bash "$MEASURE_SCRIPT" "$MODE" "$DURATION" "$RUNS_PER_BATCH" | tee "$batch_log"

    grep '^DATA|' "$batch_log" >> "$all_data"

    if (( batch < BATCHES )); then
        echo
        echo "Sleeping ${BATCH_PAUSE}s before next batch..."
        sleep "$BATCH_PAUSE"
    fi

    batch=$((batch + 1))
done

echo
echo "== Aggregated averages across all batches =="
awk -F'|' '
function avg(sum, count) {
    if (count == 0) {
        return "0.000"
    }
    return sprintf("%.3f", sum / count)
}
{
    key = $2
    rx = $4 + 0.0
    tx = $5 + 0.0
    rx_sum[key] += rx
    tx_sum[key] += tx
    count[key] += 1
}
END {
    ordered[1] = "cpp_cpp"
    ordered[2] = "rust_rust"
    ordered[3] = "rust_cpp"
    ordered[4] = "cpp_rust"
    labels["cpp_cpp"] = "C++ sender -> C++ receiver"
    labels["rust_rust"] = "Rust sender -> Rust receiver"
    labels["rust_cpp"] = "Rust sender -> C++ receiver"
    labels["cpp_rust"] = "C++ sender -> Rust receiver"

    for (i = 1; i <= 4; i++) {
        key = ordered[i]
        printf "%s: runs=%d avg_loopback_rx_mbps=%s avg_loopback_tx_mbps=%s\n",
               labels[key], count[key], avg(rx_sum[key], count[key]), avg(tx_sum[key], count[key])
    }
}
' "$all_data"