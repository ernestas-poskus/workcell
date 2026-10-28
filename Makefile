CARGO ?= cargo
RUSTDOCFLAGS ?= -D warnings
EVIDENCE ?= $(CURDIR)/target/evidence

.PHONY: fmt check clippy test doc package verify sandbox-why escape-drill

## Formatting: fail if any file is not rustfmt-clean.
fmt:
	$(CARGO) fmt --all -- --check

## Compile every target, including tests and examples.
check:
	$(CARGO) check --all-targets

## Lint every target and deny warnings.
clippy:
	$(CARGO) clippy --all-targets -- -D warnings

## Run the offline test suite (nextest) and the doc-tests.
test:
	$(CARGO) nextest run --status-level fail
	$(CARGO) test --doc

## Build the API documentation and deny rustdoc warnings.
doc:
	RUSTDOCFLAGS="$(RUSTDOCFLAGS)" $(CARGO) doc --no-deps

## Verify the package manifest and the files that would ship.
package:
	$(CARGO) package --list --allow-dirty >/dev/null

## Run every gate locally (the same set CI runs).
verify: fmt check clippy test doc package

## Evidence: assert what the rendered profiles decide. Needs `nono`, no sandbox.
sandbox-why:
	scripts/sandbox-why.sh --out "$(EVIDENCE)/sandbox-why"

## Evidence: the live escape probes. Needs a plain macOS host and a real `nono`.
escape-drill:
	$(CARGO) build --example coding_agent
	scripts/escape-drill.sh --suite all --mode both --out "$(EVIDENCE)/escape-drill"
