VERSION := $(shell grep '^version' Cargo.toml | head -1 | sed 's/.*"\(.*\)"/\1/')
MAJOR   := $(word 1, $(subst ., ,$(VERSION)))
MINOR   := $(word 2, $(subst ., ,$(VERSION)))
PATCH   := $(word 3, $(subst ., ,$(VERSION)))

.PHONY: patch minor major release

## Bump patch version (x.y.Z)
patch:
	$(eval NEW := $(MAJOR).$(MINOR).$(shell echo $$(($(PATCH)+1))))
	@$(MAKE) _bump NEW=$(NEW)

## Bump minor version (x.Y.0)
minor:
	$(eval NEW := $(MAJOR).$(shell echo $$(($(MINOR)+1))).0)
	@$(MAKE) _bump NEW=$(NEW)

## Bump major version (X.0.0)
major:
	$(eval NEW := $(shell echo $$(($(MAJOR)+1))).0.0)
	@$(MAKE) _bump NEW=$(NEW)

## Commit, tag, and push current version  (run after patch/minor/major)
release:
	$(eval V := $(VERSION))
	@echo "Releasing v$(V)"
	@git add Cargo.toml
	@git commit -m "chore: bump version to v$(V)"
	@git tag -a "v$(V)" -m "v$(V)"
	@git push origin main --tags

_bump:
	@echo "Bumping $(VERSION) → $(NEW)"
	@sed -i '' 's/^version = "$(VERSION)"/version = "$(NEW)"/' Cargo.toml
	@cargo build -q 2>&1 | grep -v "^warning" || true
