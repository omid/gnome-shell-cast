---
name: changelog
description: How to generate and write CHANGELOG.md for this project - deriving the Unreleased entries from the commits since the last tag, the Keep a Changelog 1.1.0 format, the six change types, and the antipatterns it names. Use when asked to generate or update the changelog, when adding entries for a release, or when reviewing whether an entry is written for users rather than dredged out of the commit log.
---

# The changelog

`CHANGELOG.md` follows [Keep a Changelog 1.1.0](https://keepachangelog.com/en/1.1.0/).
Read that page before a release rather than pattern-matching the existing file.

## Generating the entries

1. `git log v<last>..HEAD --no-merges --stat` for everything since the last
   release. `git tag --sort=-v:refname | head -1` names it.
2. Drop every commit a user cannot notice: refactors, tests, CI, lint, tooling,
   agent skills, dependency bumps, changelog and README edits. This is usually
   most of them.
3. Group what survives into `Added` / `Changed` / `Fixed`, in that order.
4. Write **one line per user-visible change**, not per commit — several commits
   fixing one symptom are one entry. Open with what the user saw or gets, in
   their words. Read the diff where a subject line is too terse to tell you.
5. Fold it into the existing `Unreleased` section rather than appending a second
   one, and rewrite any entry that has drifted longer than a line.

Show the result before writing it if the release is imminent; entries are hard
to fix once tagged.

## Length

One line, two at most. No paragraph explaining the mechanism, no list of the
faults that compounded — that belongs in the commit message and in
`CLAUDE.md`'s "things that have bitten us". A reader is scanning for whether
their problem is fixed.

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

- Lead with the **symptom**: users recognise "the picture was black and the cast
  dropped after a few seconds" far sooner than the name of the packet-loss bug
  behind it. Name the cause only when it changes what they should do.
- **Do not bump the version by hand.** `scripts/release.sh` computes the next
  version, rewrites `metadata.json` and `Cargo.toml` through
  `scripts/set-version.sh`, then tags and pushes; a manual bump collides with it.
- Extension and daemon ship together under one version, so one entry covers both.
