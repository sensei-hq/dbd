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

.PHONY: help bump patch minor major install _check-clean _check-ci

## Show this help.
help:
	@echo "Targets:"
	@echo "  make bump          Bump patch (default), commit, tag, push"
	@echo "  make bump patch    Same as 'make bump'"
	@echo "  make bump minor    Bump minor, commit, tag, push"
	@echo "  make bump major    Bump major, commit, tag, push"
	@echo "  make install       Install dbd into ~/.cargo/bin from working tree"
	@echo ""
	@echo "All bump targets refuse to run if the working tree has uncommitted"
	@echo "changes or local HEAD differs from origin/main, and require tests"
	@echo "and clippy to pass first."
	@echo ""
	@echo "Current version: $(VERSION)"

# Kind words exist as no-op targets so `make bump minor` parses cleanly.
patch minor major:
	@true

## Install the dbd binary into ~/.cargo/bin from the current working tree.
install:
	@cargo install --path crates/dbd-cli --locked --force

## Bump version (commits, tags, pushes). Refuses if tree is dirty or CI fails.
bump: _check-clean _check-ci
	@echo "Bumping $(VERSION) → $(NEW) ($(KIND))"
	@sed -i '' 's/^version = "$(VERSION)"/version = "$(NEW)"/' Cargo.toml
	@sed -i '' 's/"version": "[^"]*"/"version": "$(NEW)"/' site/package.json
	# Keep the pre-commit `rev:` examples in sync with the released tag.
	@sed -i '' 's/rev: v[0-9]*\.[0-9]*\.[0-9]*/rev: v$(NEW)/' README.md docs/guide/04-commands.md docs/llms/llms-full.txt
	@cargo build -q 2>&1 | grep -v "^warning" || true
	@git add Cargo.toml site/package.json README.md docs/guide/04-commands.md docs/llms/llms-full.txt
	@git commit -m "chore: bump version to v$(NEW)"
	@git tag -a "v$(NEW)" -m "v$(NEW)"
	@echo "Pushing $(BRANCH) and v$(NEW)..."
	@git push origin $(BRANCH)
	@git push origin "v$(NEW)"
	@echo "Released v$(NEW) on $(BRANCH). Now merge $(BRANCH) → main."

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

# Pre-flight: tests and clippy must pass before a release.
_check-ci:
	@echo "Running cargo test..."
	@cargo test --workspace --quiet
	@echo "Running cargo clippy..."
	@cargo clippy --workspace --all-targets --quiet -- -D warnings
	@echo "All checks passed."
