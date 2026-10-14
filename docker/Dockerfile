# Everything `make check` needs: Rust with the Cortex-M3 target, rustfmt,
# clippy, and QEMU for the Cortex-M run.
FROM rust:1
RUN rustup target add thumbv7m-none-eabi \
 && rustup component add rustfmt clippy \
 && apt-get update \
 && apt-get install -y --no-install-recommends qemu-system-arm \
 && rm -rf /var/lib/apt/lists/*
