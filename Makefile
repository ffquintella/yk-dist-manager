# yk-dist-manager — common tasks.
# See AGENTS.md for the rules these targets enforce.

.DEFAULT_GOAL := help
.PHONY: help build run run-native diagnose bundle bundle-release run-bundled \
        verify-bundle dmg pkg verify-pkg icons check check-all fmt lint test \
        test-all coverage coverage-core coverage-html hardware clean \
        release-check linux-package linux-package-release verify-package \
        windows-msi verify-msi release-notes release release-dry-run

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

pkg: bundle-release ## macOS: wrap the release .app in an installer .pkg
	packaging/macos/pkg.sh

verify-pkg: ## macOS: check the .pkg by interrogating the binary inside it
	packaging/macos/verify-pkg.sh

windows-msi: ## Windows: build the MSI (needs cargo build --release first)
	powershell -File packaging/windows/msi.ps1

verify-msi: ## Windows: install the MSI, interrogate it, uninstall (needs admin)
	powershell -File packaging/windows/verify-msi.ps1

linux-package: ## Linux: tarball (+ .deb where dpkg-deb exists) from a debug build
	packaging/linux/package.sh --deb

linux-package-release: ## Linux: the artefacts a release ships, from a release build
	packaging/linux/package.sh --release --deb

verify-package: ## Linux: check an artefact by asking the packaged binary about itself
	@artefact=$$(ls -t target/linux/*.tar.gz 2>/dev/null | head -1); \
	if [ -z "$$artefact" ]; then \
		echo "no artefact in target/linux — run: make linux-package"; exit 1; \
	fi; \
	packaging/linux/verify-package.sh "$$artefact"

release-notes: ## The notes for this version, including the schema upgrade warning
	scripts/release-notes.sh

release: ## Tag this version under releases/ and push it — starts the release build
	scripts/release.sh

release-dry-run: ## Every check `make release` makes, without creating the tag
	scripts/release.sh --dry-run

icons: ## Re-render every icon from assets/logo.svg (needs librsvg + ImageMagick)
	assets/render-icons.sh

check-lib: ## Level 1: type-check the library only — the cheapest useful command
	cargo check --lib

check: ## Fast compile check of lib + bin, default features
	cargo check

check-all: ## Compile every feature combination that ships
	cargo check                                                            # defaults: file-dialog + camera + native-device
	cargo check --no-default-features --features file-dialog               # no camera, no native transport
	cargo check --no-default-features --features file-dialog,camera        # the ykman-only build, still supported
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
	# --fail-under-lines is what makes this a gate rather than a report. Without
	# it the target printed a number and exited 0, so a change that dropped
	# coverage below the floor passed `make release-check` (AGENTS.md §4 says
	# such a change "is not ready" — now the build agrees).
	#
	# `vendor/` is excluded for a different reason than `src/ui/`: it is not this
	# project's code at all. `vendor/block` is a patched copy of a dependency
	# (features/packaging-and-release.md phase 0b), and cargo instruments a path
	# crate the way it does not instrument a registry one — so without this the
	# gate would measure somebody else's crate and move when it was updated.
	cargo llvm-cov --all-features --workspace --summary-only \
		--fail-under-lines $(COVERAGE_FLOOR) \
		--ignore-filename-regex '(src/ui/|src/app\.rs|src/main\.rs|vendor/)'

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
