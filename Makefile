# yk-dist-manager — common tasks.
# See AGENTS.md for the rules these targets enforce.

.DEFAULT_GOAL := help
.PHONY: help build run run-native diagnose bundle bundle-release run-bundled \
        verify-bundle dmg check check-all fmt lint test test-all coverage \
        coverage-core coverage-html hardware clean release-check

COVERAGE_FLOOR := 80

help: ## Show this help
	@echo "yk-dist-manager — available targets:"
	@echo
	@grep -hE '^[a-z-]+:.*?## ' $(MAKEFILE_LIST) | \
		sed 's/$$(COVERAGE_FLOOR)/$(COVERAGE_FLOOR)/' | \
		awk 'BEGIN {FS = ":.*?## "}; {printf "  \033[36m%-16s\033[0m %s\n", $$1, $$2}'
	@echo
	@echo "Database: set YKDM_DB to choose the file (a path on a share is detected)."

build: ## Debug build (default features)
	cargo build

run: ## Launch the GUI
	cargo run

run-native: ## Launch the GUI with native hardware access + encrypted-db support
	cargo run --features native-device,encrypted-db

diagnose: ## Print build, path and capability information
	cargo run --quiet -- --diagnose

bundle: ## macOS: assemble the .app (debug build)
	packaging/macos/bundle.sh

bundle-release: ## macOS: assemble the .app from a release build
	packaging/macos/bundle.sh --release

verify-bundle: ## macOS: check the assembled .app is what macOS needs
	packaging/macos/verify-bundle.sh

run-bundled: bundle ## macOS: launch the bundled app (camera scanning works here)
	open "target/bundle/YubiKey Distribution Manager.app"

dmg: ## macOS: assemble a release .app and wrap it in a .dmg
	packaging/macos/bundle.sh --release --dmg

check: ## Fast compile check, default features
	cargo check

check-all: ## Compile every feature combination that ships
	cargo check                                              # defaults: file-dialog + camera
	cargo check --no-default-features --features file-dialog # no camera code
	cargo check --features native-device
	cargo check --all-features

fmt: ## Format
	cargo fmt --all

lint: ## Clippy — must be warning-free
	cargo clippy --all-targets --all-features -- -D warnings

test: ## Unit + behaviour tests
	cargo test

test-all: ## Tests with every feature enabled
	cargo test --all-features

coverage-core: ## THE GATE: coverage of the headless core (floor: $(COVERAGE_FLOOR)%)
	cargo llvm-cov --all-features --workspace --summary-only \
		--ignore-filename-regex '(src/ui/|src/app\.rs|src/main\.rs)'

coverage: ## Whole-crate coverage, including untested egui paint code
	cargo llvm-cov --all-features --workspace --summary-only

coverage-html: ## Browsable coverage report
	cargo llvm-cov --all-features --workspace --html
	@echo "report: target/llvm-cov/html/index.html"

hardware: ## Read-only hardware tests (needs an attached YubiKey)
	cargo test --features native-device --test hardware_native -- --ignored --nocapture

release-check: fmt lint test-all coverage-core ## Everything that must pass before a release
	@echo
	@echo "Also required before tagging (AGENTS.md §5):"
	@echo "  - CHANGELOG.md: [Unreleased] moved to [x.y.z] - YYYY-MM-DD"
	@echo "  - Cargo.toml version bumped (semver; MINOR carries breaking changes while 0.x)"
	@echo "  - roadmap.md and the feature files updated"
	@echo "  - schema change? SCHEMA_VERSION bumped + migration shipped"
	@echo "  - tag vX.Y.Z — nothing is installed anywhere except from a tag"

clean: ## Remove build artefacts
	cargo clean
