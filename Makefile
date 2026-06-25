# Tools come from the nix devShell. Outside it, wrap each command in
# `nix develop --command`; inside it (IN_NIX_SHELL set), run directly.
NIX_RUN := $(if $(IN_NIX_SHELL),,nix develop --command )

.PHONY: fmt fmt-check lint test check validate run clean

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

run:
	$(NIX_RUN)cargo run

clean:
	$(NIX_RUN)cargo clean
