# Akssora

## About

Akssora is an open-source, self-hostable sandbox provider for running AI-generated and untrusted code in isolated environments.

- The project is under active development.

## Setup

**Prerequisites:** KVM support (`ls /dev/kvm` must exist).

```bash
git clone <repo-url>
cd akssora
```

**Using Make:**
```bash
make install   # downloads Firecracker, builds rootfs, fetches kernel
make build     # cargo build --workspace
make cli       # cargo run -p akssora-cli
```

**Manual equivalent:**
```bash
# Get Firecracker
ARCH="$(uname -m)"
release_url="https://github.com/firecracker-microvm/firecracker/releases"
latest=$(basename $(curl -fsSLI -o /dev/null -w %{url_effective} ${release_url}/latest))
curl -L ${release_url}/download/${latest}/firecracker-${latest}-${ARCH}.tgz | tar -xz
mv release-${latest}-$(uname -m)/firecracker-${latest}-${ARCH} firecracker

# Build the rootfs and fetch the kernel
./images/build-rootfs.sh
curl -fsSL -o images/vmlinux https://s3.amazonaws.com/spec.ccfc.min/img/quickstart_guide/x86_64/kernels/vmlinux.bin

# Build and run
cargo build --workspace
cargo run -p akssora-cli
```

## Motivation

The motivation behind Akssora started while I was building HarnessTools, a tool for running AI-generated code in a sandboxed environment. I realized there was a need for a more robust and flexible solution that could support various programming languages while providing better isolation for executing untrusted code.

I had previously built a web-based IDE in college that provided an isolated environment using Docker containers. Now, I’m building Akssora to provide a more comprehensive solution for running AI-generated code in a secure and isolated environment. It can be integrated with any AI code-generation tool to provide a seamless experience for developers and researchers.

## Why Rust

Everything that can be written in Rust will be written in Rust. So, instead of rewriting things in Rust later, I decided to start with Rust.

## Resources

Licensed under the [Apache License 2.0](https://www.apache.org/licenses/LICENSE-2.0).
