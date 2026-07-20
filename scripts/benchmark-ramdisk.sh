#!/usr/bin/env bash
set -Eeuo pipefail

readonly RAMDISK_SIZE="6G"
readonly MOUNT_POINT="${FRANKENSTEINDB_RAMDISK:-/mnt/frankensteindb-ram}"
readonly DATABASE_PATH="${MOUNT_POINT}/sutartys-benchmark"
readonly REPOSITORY_ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"

mounted=false
mount_directory_created=false

cleanup() {
    local status=$?
    trap - EXIT
    if [[ "${mounted}" == true ]]; then
        echo "unmounting ${MOUNT_POINT}" >&2
        sudo umount -- "${MOUNT_POINT}" || true
    fi
    if [[ "${mount_directory_created}" == true ]]; then
        sudo rmdir -- "${MOUNT_POINT}" 2>/dev/null || true
    fi
    exit "${status}"
}

trap cleanup EXIT

if mountpoint -q -- "${MOUNT_POINT}"; then
    echo "refusing to use existing mount: ${MOUNT_POINT}" >&2
    exit 1
fi

if [[ -d "${MOUNT_POINT}" ]] && [[ -n "$(ls -A -- "${MOUNT_POINT}")" ]]; then
    echo "refusing to mount over non-empty directory: ${MOUNT_POINT}" >&2
    exit 1
fi

echo "creating ${RAMDISK_SIZE} tmpfs at ${MOUNT_POINT}" >&2
if [[ ! -d "${MOUNT_POINT}" ]]; then
    sudo mkdir -p -- "${MOUNT_POINT}"
    mount_directory_created=true
fi
sudo mount -t tmpfs \
    -o "size=${RAMDISK_SIZE},uid=$(id -u),gid=$(id -g),mode=0700" \
    tmpfs "${MOUNT_POINT}"
mounted=true

echo "running benchmark with database ${DATABASE_PATH}" >&2
cd -- "${REPOSITORY_ROOT}"
cargo run --release --bin frankensteindb-benchmark -- \
    --database "${DATABASE_PATH}" \
    "$@"
