#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source=./common.sh
source "$SCRIPT_DIR/common.sh"
udp_prague_init_paths "$SCRIPT_DIR"

RUST_DIR="$UDP_PRAGUE_RUST_DIR"
CPP_DIR="$UDP_PRAGUE_CPP_DIR"

udp_prague_ensure_cpp_repo "$CPP_DIR"

MODE=${1:-classic}
DURATION=${2:-8}
REPORT_US=${REPORT_US:-1000000}
BASE_PORT=${BASE_PORT:-39310}
RT_FPS=${RT_FPS:-}
RT_FRAMEDURATION=${RT_FRAMEDURATION:-}
KEEP_LOGS=${KEEP_LOGS:-0}
WARNING_PATTERN=${WARNING_PATTERN:-Reset (Real-Time )?PragueCC|panic|error|Invalid|stop prague sender|Address already in use}
STARTUP_DELAY=${STARTUP_DELAY:-0.2}
PAIR_FILTER=${PAIR_FILTER:-all}
TAIL_WINDOW_SEC=${TAIL_WINDOW_SEC:-}
REPEAT_RUNS=${REPEAT_RUNS:-}
INTER_RUN_DELAY=${INTER_RUN_DELAY:-}
RUN_PORT_STRIDE=${RUN_PORT_STRIDE:-16}

case "$MODE" in
    classic)
        EXTRA_ARGS=()
        if [[ -z "$TAIL_WINDOW_SEC" ]]; then
            TAIL_WINDOW_SEC=4
        fi
        if [[ -z "$REPEAT_RUNS" ]]; then
            REPEAT_RUNS=1
        fi
        if [[ -z "$INTER_RUN_DELAY" ]]; then
            INTER_RUN_DELAY=0
        fi
        ;;
    rfc8888)
        EXTRA_ARGS=(--rfc8888)
        if [[ -z "$TAIL_WINDOW_SEC" ]]; then
            TAIL_WINDOW_SEC=4
        fi
        if [[ -z "$REPEAT_RUNS" ]]; then
            REPEAT_RUNS=1
        fi
        if [[ -z "$INTER_RUN_DELAY" ]]; then
            INTER_RUN_DELAY=0
        fi
        ;;
    rt)
        EXTRA_ARGS=(--rtmode)
        if [[ -z "$TAIL_WINDOW_SEC" ]]; then
            TAIL_WINDOW_SEC=6
        fi
        if [[ -z "$REPEAT_RUNS" ]]; then
            REPEAT_RUNS=3
        fi
        if [[ -z "$INTER_RUN_DELAY" ]]; then
            INTER_RUN_DELAY=0.2
        fi
        if [[ -n "$RT_FPS" ]]; then
            EXTRA_ARGS+=(--fps "$RT_FPS")
        fi
        if [[ -n "$RT_FRAMEDURATION" ]]; then
            EXTRA_ARGS+=(--frameduration "$RT_FRAMEDURATION")
        fi
        ;;
    *)
        echo "Unsupported mode: $MODE" >&2
        echo "Usage: $0 [classic|rfc8888|rt] [duration_seconds]" >&2
        exit 1
        ;;
esac

build_release_binaries() {
    udp_prague_build_release_binaries "$RUST_DIR" "$CPP_DIR"
}

run_logged() {
    if command -v stdbuf >/dev/null 2>&1; then
        stdbuf -oL -eL "$@"
    else
        "$@"
    fi
}

run_logged_with_timeout() {
    local duration=$1
    shift

    if command -v stdbuf >/dev/null 2>&1; then
        timeout "${duration}s" stdbuf -oL -eL "$@"
    else
        timeout "${duration}s" "$@"
    fi
}

summary_line() {
    local pattern=$1
    local path=$2
    grep -E "$pattern" "$path" | tail -n 1 || true
}

summary_time() {
    local pattern=$1
    local path=$2

    LC_ALL=C awk -v pattern="$pattern" '
        BEGIN { FS = "[ ,]+" }
        $0 ~ pattern { last = $2 }
        END {
            if (last != "") {
                print last
            }
        }
    ' "$path"
}

trailing_window_summary() {
    local pattern=$1
    local path=$2
    local window=$3
    local duration=$4

    LC_ALL=C awk -v pattern="$pattern" -v window="$window" -v duration="$duration" '
        BEGIN { FS = "[ ,]+" }

        function metric(name,    i) {
            for (i = 1; i < NF; i++) {
                if ($i == name) {
                    return $(i + 1) + 0
                }
            }
            return 0
        }

        function percent_metric(name,    i, raw, parts) {
            for (i = 1; i < NF; i++) {
                if ($i == name) {
                    raw = $(i + 1)
                    split(raw, parts, /%/)
                    return parts[1] + 0
                }
            }
            return 0
        }

        $0 ~ pattern {
            n++
            time[n] = $2 + 0
            sent[n] = metric("Sent:")
            rcvd[n] = metric("Rcvd:")
            delay[n] = metric("RTT:")
            if (delay[n] == 0) {
                delay[n] = metric("ATO:")
            }
            loss[n] = percent_metric("Lost:")
        }

        END {
            if (n == 0) {
                exit
            }

            min_t = duration - window
            if (min_t < 0) {
                min_t = 0
            }
            count = 0

            for (i = 1; i <= n; i++) {
                if (time[i] + 1e-9 >= min_t) {
                    if (count == 0) {
                        first_t = time[i]
                    }
                    count++
                    last_t = time[i]
                    sum_sent += sent[i]
                    sum_rcvd += rcvd[i]
                    sum_delay += delay[i]
                    sum_loss += loss[i]
                }
            }

            if (count == 0) {
                exit
            }

            printf "anchor=%.2f-%.2fs, covered=%.2fs, samples=%d, sent_avg=%.3f Mbps, rcvd_avg=%.3f Mbps, delay_avg=%.3f ms, loss_avg=%.2f%%", min_t, duration + 0.0, last_t - first_t, count, sum_sent / count, sum_rcvd / count, sum_delay / count, sum_loss / count
        }
    ' "$path"
}

window_metric_value() {
    local summary=$1
    local key=$2

    LC_ALL=C awk -v wanted="$key" -F', ' '
        {
            for (i = 1; i <= NF; i++) {
                split($i, parts, "=")
                if (parts[1] == wanted) {
                    value = parts[2]
                    gsub(/[^0-9.+-]/, "", value)
                    print value
                    exit
                }
            }
        }
    ' <<< "$summary"
}

record_window_metrics() {
    local prefix=$1
    local summary=$2
    local dir=$3
    local value

    [[ -n "$summary" ]] || return 0

    value=$(window_metric_value "$summary" covered)
    [[ -n "$value" ]] && echo "$value" >> "$dir/${prefix}_covered"
    value=$(window_metric_value "$summary" samples)
    [[ -n "$value" ]] && echo "$value" >> "$dir/${prefix}_samples"
    value=$(window_metric_value "$summary" sent_avg)
    [[ -n "$value" ]] && echo "$value" >> "$dir/${prefix}_sent_avg"
    value=$(window_metric_value "$summary" rcvd_avg)
    [[ -n "$value" ]] && echo "$value" >> "$dir/${prefix}_rcvd_avg"
    value=$(window_metric_value "$summary" delay_avg)
    [[ -n "$value" ]] && echo "$value" >> "$dir/${prefix}_delay_avg"
    value=$(window_metric_value "$summary" loss_avg)
    [[ -n "$value" ]] && echo "$value" >> "$dir/${prefix}_loss_avg"
}

record_coverage_metric() {
    local value=$1
    local path=$2

    [[ -n "$value" ]] || return 0
    echo "$value" >> "$path"
}

median_from_file() {
    local path=$1
    local decimals=$2

    [[ -s "$path" ]] || return 0

    LC_ALL=C sort -g "$path" | LC_ALL=C awk -v decimals="$decimals" '
        {
            values[NR] = $1
        }
        END {
            if (NR == 0) {
                exit
            }
            if (NR % 2 == 1) {
                median = values[(NR + 1) / 2]
            } else {
                median = (values[NR / 2] + values[(NR / 2) + 1]) / 2
            }
            printf "%.*f", decimals, median
        }
    '
}

format_seconds_value() {
    local value=$1

    if [[ -n "$value" ]]; then
        printf "%ss" "$value"
    else
        printf "n/a"
    fi
}

median_window_summary() {
    local prefix=$1
    local dir=$2
    local covered
    local samples
    local sent_avg
    local rcvd_avg
    local delay_avg
    local loss_avg

    covered=$(median_from_file "$dir/${prefix}_covered" 2)
    samples=$(median_from_file "$dir/${prefix}_samples" 0)
    sent_avg=$(median_from_file "$dir/${prefix}_sent_avg" 3)
    rcvd_avg=$(median_from_file "$dir/${prefix}_rcvd_avg" 3)
    delay_avg=$(median_from_file "$dir/${prefix}_delay_avg" 3)
    loss_avg=$(median_from_file "$dir/${prefix}_loss_avg" 2)

    if [[ -z "$covered$sent_avg$rcvd_avg$delay_avg$loss_avg" ]]; then
        return 0
    fi

    printf "covered=%ss, samples=%s, sent_avg=%s Mbps, rcvd_avg=%s Mbps, delay_avg=%s ms, loss_avg=%s%%" \
        "${covered:-n/a}" \
        "${samples:-n/a}" \
        "${sent_avg:-n/a}" \
        "${rcvd_avg:-n/a}" \
        "${delay_avg:-n/a}" \
        "${loss_avg:-n/a}"
}

should_run_pair() {
    local pair=$1

    case "$PAIR_FILTER" in
        all)
            return 0
            ;;
        "$pair")
            return 0
            ;;
        *)
            return 1
            ;;
    esac
}

run_pair() {
    local label=$1
    local sender_bin=$2
    local receiver_bin=$3
    local port=$4
    local metrics_dir
    local run

    metrics_dir=$(mktemp -d)

    echo
    echo "== $label =="

    for ((run = 1; run <= REPEAT_RUNS; run++)); do
        local run_port=$((port + (run - 1) * RUN_PORT_STRIDE))
        local sender_last_display
        local receiver_last_display

        run_pair_once "$sender_bin" "$receiver_bin" "$run_port"
        sender_last_display=$(format_seconds_value "$PAIR_SENDER_LAST_T")
        receiver_last_display=$(format_seconds_value "$PAIR_RECEIVER_LAST_T")

        if [[ "$REPEAT_RUNS" -gt 1 ]]; then
            echo "-- run $run/$REPEAT_RUNS (port=$run_port) --"
        fi

        echo "sender:   ${PAIR_SENDER_LAST:-<no sender summary line captured>}"
        echo "receiver: ${PAIR_RECEIVER_LAST:-<no receiver summary line captured>}"
        echo "coverage: sender_last_t=${sender_last_display} receiver_last_t=${receiver_last_display} target_duration=${DURATION}s"
        echo "sender trailing window:   ${PAIR_SENDER_WINDOW:-<no sender summary window available>}"
        echo "receiver trailing window: ${PAIR_RECEIVER_WINDOW:-<no receiver summary window available>}"

        record_coverage_metric "$PAIR_SENDER_LAST_T" "$metrics_dir/sender_last_t"
        record_coverage_metric "$PAIR_RECEIVER_LAST_T" "$metrics_dir/receiver_last_t"
        record_window_metrics sender "$PAIR_SENDER_WINDOW" "$metrics_dir"
        record_window_metrics receiver "$PAIR_RECEIVER_WINDOW" "$metrics_dir"

        if [[ -n "$PAIR_SENDER_WARN" ]]; then
            echo "sender warnings:"
            echo "$PAIR_SENDER_WARN"
        fi

        if [[ "$PAIR_SENDER_STATUS" -eq 124 ]]; then
            echo "sender exit status: 124 (timed out at ${DURATION}s; process stayed alive until the harness stop)"
        elif [[ "$PAIR_SENDER_STATUS" -ne 0 ]]; then
            echo "sender exit status: $PAIR_SENDER_STATUS"
        fi

        if [[ -n "$PAIR_RECEIVER_WARN" ]]; then
            echo "receiver warnings:"
            echo "$PAIR_RECEIVER_WARN"
        fi

        if [[ "$KEEP_LOGS" == "1" && -n "$PAIR_RUN_LOG_DIR" ]]; then
            echo "logs:     $PAIR_RUN_LOG_DIR"
        fi

        if [[ "$INTER_RUN_DELAY" != "0" && "$run" -lt "$REPEAT_RUNS" ]]; then
            sleep "$INTER_RUN_DELAY"
        fi
    done

    if [[ "$REPEAT_RUNS" -gt 1 ]]; then
        local sender_last_median
        local receiver_last_median
        local sender_window_median
        local receiver_window_median

        sender_last_median=$(median_from_file "$metrics_dir/sender_last_t" 2)
        receiver_last_median=$(median_from_file "$metrics_dir/receiver_last_t" 2)
        sender_window_median=$(median_window_summary sender "$metrics_dir")
        receiver_window_median=$(median_window_summary receiver "$metrics_dir")

        echo "median coverage: sender_last_t=$(format_seconds_value "$sender_last_median") receiver_last_t=$(format_seconds_value "$receiver_last_median") target_duration=${DURATION}s"
        echo "median sender trailing window:   ${sender_window_median:-<no sender median window available>}"
        echo "median receiver trailing window: ${receiver_window_median:-<no receiver median window available>}"
    fi

    rm -rf "$metrics_dir"
}

run_pair_once() {
    local sender_bin=$1
    local receiver_bin=$2
    local port=$3
    local tmpdir
    local receiver_pid
    local sender_status=0
    local recv_args=("-a" "0.0.0.0" "-p" "$port" "-i" "$REPORT_US")
    local send_args=("-a" "127.0.0.1" "-p" "$port" "-c" "-i" "$REPORT_US")
    local sender_pattern='^\[(RT-)?SENDER\]:'
    local receiver_pattern='^\[RECVER\]:'

    tmpdir=$(mktemp -d)

    recv_args+=("${EXTRA_ARGS[@]}")
    send_args+=("${EXTRA_ARGS[@]}")

    # Launch the receiver binary directly so `$!` refers to the real receiver
    # process. Backgrounding the `run_logged` shell function would otherwise
    # hand us the PID of a transient shell wrapper, leaving the actual receiver
    # alive after cleanup and causing later bind collisions on reused ports.
    if command -v stdbuf >/dev/null 2>&1; then
        stdbuf -oL -eL "$receiver_bin" "${recv_args[@]}" >"$tmpdir/receiver.log" 2>&1 &
    else
        "$receiver_bin" "${recv_args[@]}" >"$tmpdir/receiver.log" 2>&1 &
    fi
    receiver_pid=$!

    if [[ "$STARTUP_DELAY" != "0" ]]; then
        sleep "$STARTUP_DELAY"
    fi

    run_logged_with_timeout "$DURATION" "$sender_bin" "${send_args[@]}" >"$tmpdir/sender.log" 2>&1 || sender_status=$?

    kill "$receiver_pid" 2>/dev/null || true
    wait "$receiver_pid" 2>/dev/null || true

    PAIR_SENDER_STATUS=$sender_status
    PAIR_SENDER_LAST=$(summary_line "$sender_pattern" "$tmpdir/sender.log")
    PAIR_RECEIVER_LAST=$(summary_line "$receiver_pattern" "$tmpdir/receiver.log")
    PAIR_SENDER_LAST_T=$(summary_time "$sender_pattern" "$tmpdir/sender.log")
    PAIR_RECEIVER_LAST_T=$(summary_time "$receiver_pattern" "$tmpdir/receiver.log")
    PAIR_SENDER_WINDOW=$(trailing_window_summary "$sender_pattern" "$tmpdir/sender.log" "$TAIL_WINDOW_SEC" "$DURATION")
    PAIR_RECEIVER_WINDOW=$(trailing_window_summary "$receiver_pattern" "$tmpdir/receiver.log" "$TAIL_WINDOW_SEC" "$DURATION")
    PAIR_SENDER_WARN=$(grep -E "$WARNING_PATTERN" "$tmpdir/sender.log" || true)
    PAIR_RECEIVER_WARN=$(grep -E "$WARNING_PATTERN" "$tmpdir/receiver.log" || true)
    PAIR_RUN_LOG_DIR=

    if [[ "$KEEP_LOGS" == "1" ]]; then
        PAIR_RUN_LOG_DIR=$tmpdir
    else
        rm -rf "$tmpdir"
    fi
}

echo "== Fair comparison alignment =="
echo "mode=$MODE duration=${DURATION}s report_interval_us=$REPORT_US"
echo "repeat_runs=$REPEAT_RUNS inter_run_delay=${INTER_RUN_DELAY}s"
echo "Both ports are rebuilt in optimized mode before measurement."
echo "Each pair uses identical CLI flags, localhost, connected startup, and reports both the last summary line and a trailing ${TAIL_WINDOW_SEC}s summary window."
if [[ "$PAIR_FILTER" != "all" ]]; then
    echo "pair_filter=$PAIR_FILTER"
fi

build_release_binaries

if should_run_pair cpp-cpp; then
    run_pair \
        "C++ sender -> C++ receiver" \
        "$CPP_DIR/udp_prague_sender" \
        "$CPP_DIR/udp_prague_receiver" \
        "$BASE_PORT"
fi

if should_run_pair rust-rust; then
    run_pair \
        "Rust sender -> Rust receiver" \
        "$RUST_DIR/target/release/udp_prague_sender" \
        "$RUST_DIR/target/release/udp_prague_receiver" \
        "$((BASE_PORT + 1))"
fi

if should_run_pair rust-cpp; then
    run_pair \
        "Rust sender -> C++ receiver" \
        "$RUST_DIR/target/release/udp_prague_sender" \
        "$CPP_DIR/udp_prague_receiver" \
        "$((BASE_PORT + 2))"
fi

if should_run_pair cpp-rust; then
    run_pair \
        "C++ sender -> Rust receiver" \
        "$CPP_DIR/udp_prague_sender" \
        "$RUST_DIR/target/release/udp_prague_receiver" \
        "$((BASE_PORT + 3))"
fi