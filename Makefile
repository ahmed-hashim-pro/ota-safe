# Thin wrappers over cargo, plus the targets that regenerate what the README shows.
CARGO ?= cargo
SIM := target/release/ota-sim

.PHONY: build test lint matrix deep mutants qemu docs docs-check demo check docker-image docker-check clean

build:
	$(CARGO) build --release

test:
	$(CARGO) test

lint:
	$(CARGO) fmt --check
	$(CARGO) clippy --all-targets -- -D warnings
	cd qemu && $(CARGO) fmt --check && $(CARGO) clippy --release -- -D warnings

# Every single power cut, both workloads, all three schemes.
matrix: build
	$(SIM) matrix

# A second power cut at every operation after the first one: about 350,000
# lives, a few minutes on a laptop.
deep: build
	$(SIM) matrix --depth 2
	$(CARGO) test --release --test search -- --ignored

mutants: build
	$(SIM) mutants

# The bootloader built for a Cortex-M3, run in QEMU on every cut state.
# Needs the thumbv7m-none-eabi target and qemu-system-arm.
qemu: build
	cd qemu && $(CARGO) build --release
	scripts/qemu-check.sh

# Regenerates docs/: the chart and the tables the README quotes.
docs: build
	$(SIM) chart -o docs/cuts.svg
	$(SIM) matrix > docs/matrix.txt
	$(SIM) mutants > docs/mutants.txt

docs-check: docs
	git diff --exit-code -- docs/

check: lint test docs-check qemu

# Re-record docs/demo.gif from the real CLI with VHS.
demo: build
	docker run --rm -u $$(id -u):$$(id -g) -e HOME=/tmp -v $(CURDIR):/vhs \
	  -v $(CURDIR)/target/release:/opt/ota:ro ghcr.io/charmbracelet/vhs:v0.10.0 docs/demo.tape

# Everything above with nothing but Docker: Rust, the Cortex-M3 target and
# QEMU come from docker/Dockerfile.
docker-image:
	docker build -t ota-safe-dev -f docker/Dockerfile docker

docker-check: docker-image
	docker run --rm -u $$(id -u):$$(id -g) -e HOME=/tmp -e CARGO_HOME=/tmp/cargo \
	  -v $(CURDIR):/w -w /w ota-safe-dev make check

clean:
	$(CARGO) clean
	cd qemu && $(CARGO) clean
