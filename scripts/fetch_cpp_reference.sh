#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source=./common.sh
source "$SCRIPT_DIR/common.sh"
udp_prague_init_paths "$SCRIPT_DIR"

udp_prague_sync_cpp_repo "$UDP_PRAGUE_CPP_DIR"

echo "C++ reference checkout is ready at: $UDP_PRAGUE_CPP_DIR"
echo "Repository URL: ${UDP_PRAGUE_CPP_REPO_URL}"
echo "Pinned commit: ${UDP_PRAGUE_CPP_COMMIT}"