---
name: changelog
description: How to write CHANGELOG.md for this project - the Keep a Changelog 1.1.0 format, the six change types, and the antipatterns it names. Use when adding entries for a release, preparing one, or reviewing whether a changelog entry is written for users rather than dredged out of the commit log.
---

# Writing the changelog

`CHANGELOG.md` follows [Keep a Changelog 1.1.0](https://keepachangelog.com/en/1.1.0/).
Read that page before a release rather than pattern-matching the existing file.

## The principles that decide most questions

- **A changelog is for humans, not machines.** If an entry only makes sense to
  someone who has read the diff, rewrite it.
- **Every version gets an entry.** Skipping "boring" releases makes the whole
  file untrustworthy.
- **Group the same types of change together**; latest version first; show each
  release date.

## Shape

```markdown
## [Unreleased]

## [3] - 2026-08-14

### Fixed

- Symptom the user saw, then the cause.
```

- Heading is `## [VERSION] - YYYY-MM-DD`, ISO 8601, largest unit first. No other
  date format.
- Keep an **`Unreleased`** section at the top and move entries down into the new
  version at release time.
- A withdrawn release keeps its entry and is marked `[YANKED]`.
- Versions and sections should be **linkable**.

## The six headings — use only these

| Heading | For |
|---|---|
| `Added` | new features |
| `Changed` | changes to existing behaviour |
| `Deprecated` | soon-to-be removed features |
| `Removed` | features taken out |
| `Fixed` | bug fixes |
| `Security` | vulnerabilities |

## What the guide explicitly says not to do

- **Do not dump commit logs.** Merge commits and terse subjects are noise; they
  are not a description of what changed for a user.
- **Do not skip deprecations and breaking changes.** Hiding them forces a
  painful upgrade.
- **Do not use ambiguous dates.** ISO 8601 only.
- **Do not document changes selectively** - a partial changelog is called out as
  "as dangerous as not having a changelog".

## For this project specifically

- Lead with the **symptom**, then the cause: users recognise "the picture was
  black and the cast dropped after a few seconds" far sooner than the name of
  the packet-loss bug behind it.
- **Leave out what users cannot see** - lint configuration, refactors, tooling,
  agent skills. Listing them buries the entries that matter.
- **Do not bump the version by hand.** `scripts/release.sh` computes the next
  version, rewrites `metadata.json` and `Cargo.toml` through
  `scripts/set-version.sh`, then tags and pushes; a manual bump collides with it.
- Extension and daemon ship together under one version, so one entry covers both.
