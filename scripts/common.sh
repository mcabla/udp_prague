#!/usr/bin/env bash

udp_prague_has_cpp_sources() {
    local cpp_dir=$1

    [[ -f "$cpp_dir/Makefile" && -f "$cpp_dir/udp_prague_sender.cpp" && -f "$cpp_dir/udp_prague_receiver.cpp" ]]
}

udp_prague_has_cpp_repo() {
    local cpp_dir=$1

    udp_prague_has_cpp_sources "$cpp_dir" && git -C "$cpp_dir" rev-parse --is-inside-work-tree >/dev/null 2>&1
}

udp_prague_require_clean_cpp_repo() {
    local cpp_dir=$1

    if [[ -n "$(git -C "$cpp_dir" status --short --untracked-files=no)" ]]; then
        echo "Refusing to move $cpp_dir because it has local tracked changes." >&2
        echo "Commit or stash those changes, or point UDP_PRAGUE_CPP_DIR at a clean checkout." >&2
        return 1
    fi
}

udp_prague_checkout_cpp_repo_commit() {
    local cpp_dir=$1
    local commit=${2:-$UDP_PRAGUE_CPP_COMMIT}

    if ! udp_prague_has_cpp_repo "$cpp_dir"; then
        echo "Cannot select C++ reference commit in $cpp_dir because it does not look like the upstream udp_prague checkout." >&2
        return 1
    fi

    if ! command -v git >/dev/null 2>&1; then
        echo "git is required to select the optional C++ reference commit in $cpp_dir." >&2
        return 1
    fi

    udp_prague_require_clean_cpp_repo "$cpp_dir"

    if ! git -C "$cpp_dir" rev-parse --verify "$commit^{commit}" >/dev/null 2>&1; then
        echo "Commit $commit is not available in $cpp_dir." >&2
        echo "Run bash scripts/fetch_cpp_reference.sh to fetch the pinned reference commit." >&2
        return 1
    fi

    if [[ "$(git -C "$cpp_dir" rev-parse HEAD)" != "$commit" ]]; then
        echo "== Checking out pinned C++ reference commit $commit =="
        git -C "$cpp_dir" -c advice.detachedHead=false checkout --detach "$commit" >/dev/null
    fi
}

udp_prague_clone_cpp_repo() {
    local cpp_dir=$1
    local repo_url=$2
    local commit=${3:-$UDP_PRAGUE_CPP_COMMIT}

    if [[ -e "$cpp_dir" ]]; then
        if udp_prague_has_cpp_sources "$cpp_dir"; then
            echo "Expected a git checkout at $cpp_dir, but found plain udp_prague sources without git metadata." >&2
            echo "Point UDP_PRAGUE_CPP_DIR at a real checkout or remove $cpp_dir so the helper can clone one." >&2
            return 1
        fi

        echo "Expected a clone target at $cpp_dir, but that path already exists and does not look like the upstream C++ reference repo." >&2
        return 1
    fi

    if ! command -v git >/dev/null 2>&1; then
        echo "git is required to clone the optional C++ reference repo into $cpp_dir." >&2
        return 1
    fi

    mkdir -p -- "$(dirname -- "$cpp_dir")"
    echo "== Cloning C++ reference checkout into $cpp_dir =="
    git clone "$repo_url" "$cpp_dir"

    if ! udp_prague_has_cpp_sources "$cpp_dir"; then
        echo "Cloned $cpp_dir, but it does not contain the expected upstream udp_prague sources." >&2
        return 1
    fi

    udp_prague_checkout_cpp_repo_commit "$cpp_dir" "$commit"
}

udp_prague_update_cpp_repo() {
    local cpp_dir=$1
    local commit=${2:-$UDP_PRAGUE_CPP_COMMIT}

    if ! udp_prague_has_cpp_repo "$cpp_dir"; then
        echo "Cannot update $cpp_dir because it does not look like the upstream C++ reference repo checkout." >&2
        return 1
    fi

    if ! command -v git >/dev/null 2>&1; then
        echo "git is required to update the optional C++ reference repo in $cpp_dir." >&2
        return 1
    fi

    udp_prague_require_clean_cpp_repo "$cpp_dir"

    echo "== Fetching C++ reference checkout in $cpp_dir =="
    git -C "$cpp_dir" fetch --prune origin >/dev/null
    udp_prague_checkout_cpp_repo_commit "$cpp_dir" "$commit"
}

udp_prague_sync_cpp_repo() {
    local cpp_dir=$1
    local repo_url=${2:-$UDP_PRAGUE_CPP_REPO_URL}
    local commit=${3:-$UDP_PRAGUE_CPP_COMMIT}

    if udp_prague_has_cpp_repo "$cpp_dir"; then
        udp_prague_update_cpp_repo "$cpp_dir" "$commit"
        return 0
    fi

    if [[ "${UDP_PRAGUE_AUTO_CLONE_CPP:-1}" != "1" ]]; then
        echo "Missing optional C++ reference repo at $cpp_dir." >&2
        echo "Run bash scripts/fetch_cpp_reference.sh or set UDP_PRAGUE_CPP_DIR to an existing checkout." >&2
        return 1
    fi

    udp_prague_clone_cpp_repo "$cpp_dir" "$repo_url" "$commit"
}

udp_prague_ensure_cpp_repo() {
    local cpp_dir=$1
    local repo_url=${2:-$UDP_PRAGUE_CPP_REPO_URL}
    local commit=${3:-$UDP_PRAGUE_CPP_COMMIT}

    udp_prague_sync_cpp_repo "$cpp_dir" "$repo_url" "$commit"
}

udp_prague_init_paths() {
    local script_dir=$1
    local repo_local_cpp_dir

    UDP_PRAGUE_SCRIPT_DIR=$script_dir
    UDP_PRAGUE_RUST_DIR=$(cd -- "$script_dir/.." && pwd)
    UDP_PRAGUE_CPP_REPO_URL=${UDP_PRAGUE_CPP_REPO_URL:-https://github.com/L4STeam/udp_prague.git}
    UDP_PRAGUE_CPP_COMMIT=${UDP_PRAGUE_CPP_COMMIT:-e8e343533a7cc39b40e357b3975a557a081bf6ec}

    repo_local_cpp_dir="$UDP_PRAGUE_RUST_DIR/udp_prague"

    if [[ -n "${UDP_PRAGUE_CPP_DIR:-}" ]]; then
        UDP_PRAGUE_CPP_DIR=${UDP_PRAGUE_CPP_DIR}
    elif udp_prague_has_cpp_repo "$repo_local_cpp_dir"; then
        UDP_PRAGUE_CPP_DIR=$repo_local_cpp_dir
    else
        UDP_PRAGUE_CPP_DIR=$repo_local_cpp_dir
    fi
}

udp_prague_build_release_binaries() {
    local rust_dir=$1
    local cpp_dir=$2

    udp_prague_sync_cpp_repo "$cpp_dir"

    echo "== Building optimized binaries =="
    (
        cd "$rust_dir"
        cargo build --release >/dev/null
    )
    (
        cd "$cpp_dir"
        make -j2 >/dev/null
    )
}