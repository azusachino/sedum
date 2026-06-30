# Tools come from the nix devShell. Outside it, wrap each command in
# `nix develop --command`; inside it (IN_NIX_SHELL set), run directly.
NIX_RUN := $(if $(IN_NIX_SHELL),,nix develop --command )

.PHONY: fmt fmt-check lint test check validate bench run clean daily stack-up stack-down stack-build stack-logs

fmt:
	$(NIX_RUN)cargo fmt
	$(NIX_RUN)prettier --write "**/*.{json,yaml,yml}"

fmt-check:
	$(NIX_RUN)cargo fmt -- --check
	$(NIX_RUN)prettier --check "**/*.{json,yaml,yml}"

lint:
	$(NIX_RUN)cargo clippy --all-targets -- -D warnings

test:
	$(NIX_RUN)cargo test

check: fmt-check lint test

validate: check
	$(NIX_RUN)cargo build --release

bench:
	$(NIX_RUN)uv run python scripts/index_scale_test.py

run:
	$(NIX_RUN)cargo run

clean:
	$(NIX_RUN)cargo clean

# Daily target: run quality checks and rebuild the local stack image
daily: check stack-build

# Local stack operations
stack-up:
	podman compose up -d

stack-down:
	podman compose down

stack-build:
	podman compose up -d --build --force-recreate miku

stack-logs:
	podman compose logs -f
