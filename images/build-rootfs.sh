#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")"

dd if=/dev/zero of=rootfs.ext4 bs=1M count=50
mkfs.ext4 rootfs.ext4

mkdir -p /tmp/akssora-rootfs
sudo mount rootfs.ext4 /tmp/akssora-rootfs

docker run --rm -v /tmp/akssora-rootfs:/my-rootfs alpine sh -c \
  "cp -r /bin /etc /lib /sbin /usr /my-rootfs/ && mkdir -p /my-rootfs/dev /my-rootfs/proc /my-rootfs/sys /my-rootfs/tmp"

# NEW — copy in the guest agent binary
sudo cp ../target/x86_64-unknown-linux-musl/release/akssora-guest-agent /tmp/akssora-rootfs/akssora-guest-agent
sudo chmod +x /tmp/akssora-rootfs/akssora-guest-agent

sudo umount /tmp/akssora-rootfs

echo "rootfs built: $(ls -lh rootfs.ext4)"