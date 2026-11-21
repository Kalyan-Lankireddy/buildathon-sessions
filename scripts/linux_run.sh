#!/bin/sh
# Quick Linux timing loop on a Mac: run the linux/arm64 image (native speed in Docker's VM) on a
# dataset stored in a Docker volume (ext4, no virtiofs). Usage: linux_run.sh VOLUME [CPUS] [MEM]
# Build the image first:  docker buildx build --platform linux/arm64 --build-arg RUSTFLAGS="" --load -t shopping-sessions:arm64 .
set -eu
VOL="$1"; CPUS="${2:-8}"; MEM="${3:-15g}"
docker run --rm --platform linux/arm64 --cpus "$CPUS" --memory "$MEM" --memory-swap "$MEM" \
  --user 1000:1000 --read-only --tmpfs /tmp:rw,size=64m --tmpfs /output:rw,size=2g,mode=1777 \
  -e NCPU="$CPUS" ${NOTRIM:+-e NOTRIM=1} -v "$VOL":/data:ro ${IMG:-shopping-sessions:arm64}
