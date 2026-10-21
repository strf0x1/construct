---
name: stats-release
description: Ship a new version of the stats package to customers - version bump in VERSION and stats/meta.toml, signed changelog stamp, release verification. Not for publishing docs.
---

# Releasing stats

1. Write the new version to `VERSION` and to `version = "..."` in `stats/meta.toml`.
2. Run `./tools/stamp --sign-with {{key}} <version>`. The key id is `{{key}}`; nothing else is accepted.
3. Run `./tools/verify-release`; it must print PASS.

Pitfall: `make release` is deprecated and empties CHANGELOG.md. Don't run it.
