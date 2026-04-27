#!/usr/bin/env bash
set -euo pipefail
export LC_ALL=C

SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source=./common.sh
source "$SCRIPT_DIR/common.sh"
udp_prague_init_paths "$SCRIPT_DIR"

RUST_DIR="$UDP_PRAGUE_RUST_DIR"
CPP_DIR="$UDP_PRAGUE_CPP_DIR"

udp_prague_ensure_cpp_repo "$CPP_DIR"

MODE=${1:-classic}
DURATION=${2:-8}
REPEATS=${3:-3}
BASE_PORT=${BASE_PORT:-39410}
WARMUP=${WARMUP:-3}
STARTUP_DELAY=${STARTUP_DELAY:-0.2}
RUN_GAP=${RUN_GAP:-1}
SKIP_BUILD=${SKIP_BUILD:-0}
EMIT_PARSE_LINES=${EMIT_PARSE_LINES:-0}
RFC8888_ACKPERIOD_US=${RFC8888_ACKPERIOD_US:-25000}
LO_RX=/sys/class/net/lo/statistics/rx_bytes
LO_TX=/sys/class/net/lo/statistics/tx_bytes

case "$MODE" in
    classic)
        EXTRA_ARGS=()
        ;;
    rfc8888)
        EXTRA_ARGS=(--rfc8888 --rfc8888ackperiod "$RFC8888_ACKPERIOD_US")
        ;;
    *)
        echo "Unsupported mode: $MODE" >&2
        echo "Usage: $0 [classic|rfc8888] [duration_seconds] [repeats]" >&2
        exit 1
        ;;
esac

if [[ ! -r "$LO_RX" || ! -r "$LO_TX" ]]; then
    echo "Loopback byte counters are not readable; cannot run quiet throughput measurement." >&2
    exit 1
fi

build_release_binaries() {
    udp_prague_build_release_binaries "$RUST_DIR" "$CPP_DIR"
}

mbps() {
    local bytes=$1
    local seconds=$2
    awk -v bytes="$bytes" -v seconds="$seconds" 'BEGIN { printf "%.3f", (bytes * 8.0) / (seconds * 1000000.0) }'
}

median_of() {
    local values=("$@")
    local sorted=()
    local count=${#values[@]}
    local mid

    mapfile -t sorted < <(printf '%s\n' "${values[@]}" | sort -n)

    if (( count % 2 == 1 )); then
        mid=$((count / 2))
        printf '%s' "${sorted[$mid]}"
    else
        mid=$((count / 2))
        awk -v a="${sorted[$((mid - 1))]}" -v b="${sorted[$mid]}" 'BEGIN { printf "%.3f", (a + b) / 2.0 }'
    fi
}

run_pair_once() {
    local sender_bin=$1
    local receiver_bin=$2
    local port=$3

    local recv_args=("-a" "0.0.0.0" "-p" "$port" "-q")
    local send_args=("-a" "127.0.0.1" "-p" "$port" "-c" "-q")

    recv_args+=("${EXTRA_ARGS[@]}")
    send_args+=("${EXTRA_ARGS[@]}")

    local rx_before tx_before rx_after tx_after
    "$receiver_bin" "${recv_args[@]}" >/dev/null 2>&1 &
    local receiver_pid=$!

    timeout "$((WARMUP + DURATION + 5))s" "$sender_bin" "${send_args[@]}" >/dev/null 2>&1 &
    local sender_pid=$!

    cleanup() {
        kill "$sender_pid" 2>/dev/null || true
        wait "$sender_pid" 2>/dev/null || true
        kill "$receiver_pid" 2>/dev/null || true
        wait "$receiver_pid" 2>/dev/null || true
    }
    trap cleanup RETURN

    # Give the receiver a short moment to bind consistently.
    sleep "$STARTUP_DELAY"

    # Let the continuous sender/receiver pair warm up before sampling.
    sleep "$WARMUP"

    rx_before=$(<"$LO_RX")
    tx_before=$(<"$LO_TX")

    sleep "$DURATION"

    rx_after=$(<"$LO_RX")
    tx_after=$(<"$LO_TX")

    kill "$sender_pid" 2>/dev/null || true
    wait "$sender_pid" 2>/dev/null || true
    kill "$receiver_pid" 2>/dev/null || true
    wait "$receiver_pid" 2>/dev/null || true

    local delta_rx=$((rx_after - rx_before))
    local delta_tx=$((tx_after - tx_before))

    printf '%s %s\n' "$delta_rx" "$delta_tx"
}

run_pair() {
    local key=$1
    local label=$2
    local sender_bin=$3
    local receiver_bin=$4
    local port=$5
    local rx_results=()
    local tx_results=()

    echo
    echo "== $label =="

    local repeat
    for repeat in $(seq 1 "$REPEATS"); do
        local stats delta_rx delta_tx
        stats=$(run_pair_once "$sender_bin" "$receiver_bin" "$port")
        read -r delta_rx delta_tx <<<"$stats"
        local rx_mbps tx_mbps
        rx_mbps=$(mbps "$delta_rx" "$DURATION")
        tx_mbps=$(mbps "$delta_tx" "$DURATION")
        rx_results+=("$rx_mbps")
        tx_results+=("$tx_mbps")
        echo "run_$repeat: loopback_rx_mbps=$rx_mbps loopback_tx_mbps=$tx_mbps"
        if [[ "$EMIT_PARSE_LINES" == "1" ]]; then
            printf 'DATA|%s|%s|%s|%s\n' "$key" "$repeat" "$rx_mbps" "$tx_mbps"
        fi
        sleep "$RUN_GAP"
    done

    local median_rx median_tx
    median_rx=$(median_of "${rx_results[@]}")
    median_tx=$(median_of "${tx_results[@]}")
    echo "median: loopback_rx_mbps=$median_rx loopback_tx_mbps=$median_tx"
    if [[ "$EMIT_PARSE_LINES" == "1" ]]; then
        printf 'MEDIAN|%s|%s|%s\n' "$key" "$median_rx" "$median_tx"
    fi
}

echo "== Fair performance alignment =="
echo "mode=$MODE duration=${DURATION}s repeats=$REPEATS warmup=${WARMUP}s"
echo "Both ports are rebuilt in optimized mode before measurement."
echo "Each pair runs in quiet mode so periodic summaries and JSON logging are not part of the measured path."
echo "Loopback interface byte counters provide the throughput measurement."

if [[ "$SKIP_BUILD" != "1" ]]; then
    build_release_binaries
fi

run_pair \
    "cpp_cpp" \
    "C++ sender -> C++ receiver" \
    "$CPP_DIR/udp_prague_sender" \
    "$CPP_DIR/udp_prague_receiver" \
    "$BASE_PORT"

run_pair \
    "rust_rust" \
    "Rust sender -> Rust receiver" \
    "$RUST_DIR/target/release/udp_prague_sender" \
    "$RUST_DIR/target/release/udp_prague_receiver" \
    "$((BASE_PORT + 1))"

run_pair \
    "rust_cpp" \
    "Rust sender -> C++ receiver" \
    "$RUST_DIR/target/release/udp_prague_sender" \
    "$CPP_DIR/udp_prague_receiver" \
    "$((BASE_PORT + 2))"

run_pair \
    "cpp_rust" \
    "C++ sender -> Rust receiver" \
    "$CPP_DIR/udp_prague_sender" \
    "$RUST_DIR/target/release/udp_prague_receiver" \
    "$((BASE_PORT + 3))"