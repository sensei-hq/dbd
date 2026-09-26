VERSION := $(shell grep '^version' Cargo.toml | head -1 | sed 's/.*"\(.*\)"/\1/')
# `make bump` runs on the current branch (typically develop) and pushes it;
# the version commit/tag then reaches main via the usual develop → main merge.
BRANCH  := $(shell git rev-parse --abbrev-ref HEAD)
MAJOR   := $(word 1, $(subst ., ,$(VERSION)))
MINOR   := $(word 2, $(subst ., ,$(VERSION)))
PATCH   := $(word 3, $(subst ., ,$(VERSION)))

# `make bump` defaults to patch. Pass `patch` / `minor` / `major` as an
# extra word to pick the kind, e.g. `make bump minor`. The kind words are
# defined as no-op phony targets so make doesn't complain.
KIND := patch
ifneq (,$(filter major,$(MAKECMDGOALS)))
  KIND := major
endif
ifneq (,$(filter minor,$(MAKECMDGOALS)))
  KIND := minor
endif

ifeq ($(KIND),major)
  NEW := $(shell echo $$(($(MAJOR)+1))).0.0
else ifeq ($(KIND),minor)
  NEW := $(MAJOR).$(shell echo $$(($(MINOR)+1))).0
else
  NEW := $(MAJOR).$(MINOR).$(shell echo $$(($(PATCH)+1)))
endif

# Builds invoked through this Makefile (bump's test/clippy/build) run once and
# then get wiped, so incremental compilation only writes cache we immediately
# delete. Disable it for all make-driven cargo builds — day-to-day dev uses
# plain `cargo` and is unaffected.
export CARGO_INCREMENTAL := 0

.PHONY: help bump patch minor major install clean sweep _check-clean _check-ci

# Install from the working tree, then reclaim target/, leaving the install's own
# status in `ok` for the caller to act on. Shared verbatim by `install` and the
# post-push step of `bump`, which must end in the same state.
#
# The clean is deliberately in the SAME shell statement as the install: if it
# were a separate recipe line, a failed install would abort the recipe and skip
# it — and in `bump` that strands the tree *after* the tag is already public.
# The clean's own exit status is inspected rather than assumed, so "target/
# cleaned" is never printed over a clean that did not happen.
#
# Shared as a plain variable and NOT as a recursive `$(MAKE) install` on purpose:
# make executes any recipe line containing `$(MAKE)` even under `-n`, so a
# recursive call here would make `make -n bump` really wipe target/. Both
# `a_dry_run_bump_does_not_touch_the_build_dir` and
# `a_dry_run_install_does_not_touch_the_build_dir` pin that by side effect.
define INSTALL_AND_RECLAIM
cargo install --path . --locked --force; ok=$$?; \
	 echo "Reclaiming disk: removing debug + stale build artifacts..."; \
	 if cargo clean; then \
	   echo "target/ cleaned; next build recompiles against the current lockfile."; \
	 else \
	   echo "WARNING: cargo clean failed — target/ is still on disk."; \
	   if [ $$ok -eq 0 ]; then ok=1; fi; \
	 fi
endef

## Show this help.
help:
	@echo "Targets:"
	@echo "  make bump          Bump patch (default), commit, tag, push (CI then publishes)"
	@echo "  make bump patch    Same as 'make bump'"
	@echo "  make bump minor    Bump minor, commit, tag, push"
	@echo "  make bump major    Bump major, commit, tag, push"
	@echo "  make install       Install dbd into ~/.cargo/bin, then reclaim target/"
	@echo "  make clean         Remove the target/ build directory (cargo clean)"
	@echo "  make sweep         Prune stale/old-version artifacts (needs cargo-sweep)"
	@echo ""
	@echo "All bump targets refuse to run if the working tree has uncommitted"
	@echo "changes or local HEAD differs from origin/<current branch>, and require"
	@echo "tests, clippy and rustfmt to pass first. After a successful push, bump"
	@echo "installs the released binary into ~/.cargo/bin, then runs"
	@echo "'cargo clean' to reclaim disk (next dev build recompiles fresh)."
	@echo ""
	@echo "Current version: $(VERSION)"

# Kind words exist as no-op targets so `make bump minor` parses cleanly.
patch minor major:
	@true

## Install the dbd binary into ~/.cargo/bin from the current working tree,
## then reclaim target/ — `cargo install --path .` builds there, so leaving it
## behind is ~1 GB the command itself created.
install:
	@trap 'echo ""; echo "Interrupted. Re-run: make install"; exit 130' INT; \
	 $(INSTALL_AND_RECLAIM); \
	 if [ $$ok -ne 0 ]; then exit $$ok; fi; \
	 echo "dbd is on your PATH."

## Remove the target/ build directory to reclaim disk space.
clean:
	@cargo clean

## Prune stale build artifacts (other toolchains + files untouched for 14 days),
## keeping the current working set so the next build stays warm. Needs cargo-sweep.
sweep:
	@if ! command -v cargo-sweep >/dev/null 2>&1; then \
	  echo "cargo-sweep not installed. Install it with:"; \
	  echo "  cargo install cargo-sweep"; \
	  echo "Or run 'make clean' to wipe target/ entirely."; \
	  exit 1; \
	fi
	@cargo sweep --installed
	@cargo sweep --time 14

## Bump version (commits, tags, pushes). Refuses if tree is dirty or CI fails.
##
## Notes on the recipe below, kept out of it because make echoes un-@'d comment
## lines in a recipe and they would print on every release:
##
##  - The `sed` over README/guide/llms keeps the pre-commit `rev:` examples in
##    sync with the tag being cut.
##  - `install` runs *after* the push (so only what actually shipped lands on
##    PATH) but *before* `cargo clean`. `cargo install --path` reuses the
##    workspace target/, so the artifacts it builds are wiped by the clean that
##    follows; installing after the clean instead strands a fresh target/release.
##  - The two share one shell statement so the clean is unconditional. Left as
##    separate recipe lines, a failed install aborts the rule and leaves target/
##    behind, with the tag already public and no way to finish the release —
##    re-running `make bump` would cut a new version rather than resume this one.
##  - It spells out `cargo install` rather than recursing into the `install`
##    target: make runs any recipe line containing `$(MAKE)` even under `-n`, so
##    `$(MAKE) install` here would make a dry run really execute the `cargo
##    clean` that shares this statement, wiping target/ from a supposed no-op.
bump: _check-clean _check-ci
	@echo "Bumping $(VERSION) → $(NEW) ($(KIND))"
	@sed -i '' 's/^version = "$(VERSION)"/version = "$(NEW)"/' Cargo.toml
	@sed -i '' 's|dbd-core = { path = "crates/dbd-core", version = "$(VERSION)" }|dbd-core = { path = "crates/dbd-core", version = "$(NEW)" }|' Cargo.toml
	@sed -i '' 's/"version": "[^"]*"/"version": "$(NEW)"/' site/package.json
##  The website serves a COPY of every guide page and the llms files, so the
##  rewrite has to reach both or the published site keeps the old pin. It did
##  exactly that: the site said `rev: v0.13.0` six releases after docs/ moved
##  on, because only the docs/ path was listed here.
	@sed -i '' 's/rev: v[0-9]*\.[0-9]*\.[0-9]*/rev: v$(NEW)/' README.md docs/guide/04-commands.md docs/llms/llms-full.txt \
	  site/src/lib/content/guide/04-commands.md site/src/lib/content/llms/llms-full.txt
##  sensei.library.json carries two version-coupled fields that consumers rely on
##  to answer "are these docs for the release I depend on?": `documents` (the
##  concrete release the published docs describe) and `ref` (the tag to fetch the
##  corpus at). Both are wrong the moment a release ships without them, and wrong
##  here is worse than absent — a consumer trusts the label and serves docs for
##  the wrong version silently. `version` is a RANGE (">=0.10") and is
##  deliberately NOT touched: it says which releases the capabilities apply to,
##  which is a different question and does not move every bump.
	@sed -i '' 's/"documents": "[^"]*"/"documents": "$(NEW)"/' sensei.library.json
	@sed -i '' 's/"ref": "v[0-9]*\.[0-9]*\.[0-9]*"/"ref": "v$(NEW)"/' sensei.library.json
	@cargo build -q
	@git add Cargo.lock Cargo.toml site/package.json README.md docs/guide/04-commands.md docs/llms/llms-full.txt \
	  site/src/lib/content/guide/04-commands.md site/src/lib/content/llms/llms-full.txt sensei.library.json
	@git commit -m "chore: bump version to v$(NEW)"
	@git tag -a "v$(NEW)" -m "v$(NEW)"
	@echo "Pushing $(BRANCH) and v$(NEW)..."
	@git push origin $(BRANCH) && git push origin "v$(NEW)" || { \
	   echo ""; \
	   echo "Push incomplete. If the branch landed but the tag did not, resume with:"; \
	   echo "    git push origin v$(NEW)"; \
	   echo "Do NOT re-run 'make bump' — the version is already committed, so it"; \
	   echo "would cut $(NEW)+1 and leave v$(NEW) untagged forever."; \
	   exit 1; \
	 }
	@echo "Released v$(NEW) on $(BRANCH). Now merge $(BRANCH) → main."
	@echo "Pushing the tag triggers .github/workflows/release.yml, which re-runs the"
	@echo "suite on the tagged tree and publishes to crates.io. Watch it with:"
	@echo "    gh run list --workflow=release.yml"
	@echo "If it fails, re-run it on the same tag (never delete and re-push a tag):"
	@echo "    gh workflow run release.yml -f tag=v$(NEW)"
	@echo "Installing v$(NEW) into ~/.cargo/bin..."
	@trap 'echo ""; echo "Interrupted. v$(NEW) is tagged and pushed, so the release itself is"; echo "complete. Run: make install   (do NOT run make bump)"; exit 130' INT; \
	 $(INSTALL_AND_RECLAIM); \
	 if [ $$ok -ne 0 ]; then \
	   echo ""; \
	   echo "v$(NEW) is tagged and pushed — the release itself is complete."; \
	   echo "Only the local step failed. Re-run 'make install' once resolved;"; \
	   echo "do NOT re-run 'make bump', which would cut another version."; \
	   exit $$ok; \
	 fi; \
	 echo "dbd v$(NEW) is on your PATH."

# Refuse to bump if the working tree has uncommitted changes or untracked
# files. Also require local HEAD to be in sync with origin/<current branch>
# so we don't tag a commit the remote can't fast-forward to.
_check-clean:
	@if [ -n "$$(git status --porcelain)" ]; then \
	  echo "Refusing to bump: working tree has uncommitted changes."; \
	  echo ""; \
	  git status --short; \
	  echo ""; \
	  echo "Commit or stash, then re-run."; \
	  exit 1; \
	fi
	@git fetch --quiet origin $(BRANCH)
	@LOCAL=$$(git rev-parse HEAD); \
	 REMOTE=$$(git rev-parse origin/$(BRANCH)); \
	 if [ "$$LOCAL" != "$$REMOTE" ]; then \
	   echo "Refusing to bump: local HEAD ($$LOCAL) differs from origin/$(BRANCH) ($$REMOTE)."; \
	   echo "Push or pull first."; \
	   exit 1; \
	 fi

# Pre-flight: tests, clippy and formatting must pass before a release.
_check-ci:
	@echo "Running cargo test..."
	@cargo test --workspace --quiet
	@echo "Running cargo clippy..."
	@cargo clippy --workspace --all-targets --quiet -- -D warnings
	@echo "Running cargo fmt --check..."
	@cargo fmt --all --check
	@echo "All checks passed."
