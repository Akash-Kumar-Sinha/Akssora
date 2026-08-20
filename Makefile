.PHONY: install build guest-agent rootfs cli run install-cli clean

install:
	@ARCH="$$(uname -m)"; \
	release_url="https://github.com/firecracker-microvm/firecracker/releases"; \
	latest=$$(basename $$(curl -fsSLI -o /dev/null -w %{url_effective} $${release_url}/latest)); \
	curl -L $${release_url}/download/$${latest}/firecracker-$${latest}-$${ARCH}.tgz | tar -xz; \
	mv release-$${latest}-$$(uname -m)/firecracker-$${latest}-$${ARCH} firecracker
	rustup target add x86_64-unknown-linux-musl
	curl -fsSL -o images/vmlinux https://s3.amazonaws.com/spec.ccfc.min/img/quickstart_guide/x86_64/kernels/vmlinux.bin

# Cross-compile the guest agent for musl — must happen before rootfs build,
# since build-rootfs.sh copies this binary in.
guest-agent:
	cargo build -p akssora-guest-agent --release --target x86_64-unknown-linux-musl

rootfs: guest-agent
	./images/build-rootfs.sh

build:
	cargo build --workspace

cli:
	cargo run -p akssora-cli -- $(ARGS)

install-cli:
	cargo install --path crates/akssora-cli

clean:
	rm -f /tmp/firecracker.socket /tmp/firecracker.vsock