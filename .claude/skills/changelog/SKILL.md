---
name: changelog
description: How to generate and write CHANGELOG.md for this project - deriving the Unreleased entries from the commits since the last tag, the Keep a Changelog 1.1.0 format, the six change types, and how to word an entry so a user can tell in one short line whether it affects them and what to do next. Use when asked to generate or update the changelog, when adding entries for a release, or when reviewing whether an entry is written for users rather than dredged out of the commit log.
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
4. For each survivor ask **"what changes for someone using this?"** and write
   that, one line per user-visible change rather than per commit — several
   commits fixing one symptom are one entry. Read the diff where a subject line
   is too terse to tell you. See "How an entry reads" below for the wording.
5. Fold it into the existing `Unreleased` section rather than appending a second
   one, and rewrite any entry that has drifted longer than a line.

Show the result before writing it if the release is imminent; entries are hard
to fix once tagged.

## How an entry reads

The reader is a user scanning for whether their problem is fixed, or whether
this release gives them anything. Write for that, in their words.

    <what they see or get>[, <what they can now do>]

- **One line. Aim under 15 words**, two lines only when the second one tells
  them to do something.
- **Present tense, plain words.** "Casting no longer drops", not "Fixed an
  issue where the session would terminate".
- **Say the payoff, not the mechanism.** No paragraph explaining how it broke,
  no list of the faults that compounded — that belongs in the commit message
  and in `CLAUDE.md`'s "things that have bitten us".
- **Keep internals out**: package, element, protocol and file names mean
  nothing to a user. Name one only when they have to act on it, and then say
  what to do with it.
- **Give them the action when there is one.** A fix that needs a package
  installed, or a setting changed, is not useful until the entry says so.

### The test

Read the entry as someone who has never seen the code. Can they tell whether
it affects them, and what to do next? If not, rewrite it.

### Before and after

| Drifted | Rewritten |
|---|---|
| Casting failed to start at all on machines with VA-API installed (`gst-plugin-va`): the hardware H.264 encoder was offered to the device and then could not be used. | Casting no longer fails to start on Intel and AMD graphics. |
| NixOS support: a Nix flake builds both halves, with the GStreamer plugins and `pactl` bundled into the daemon so no session setup is needed. | NixOS support: install from the flake, no extra setup. |
| Cast details now name the encoder and pixel format actually in use, and say whether that encoder is hardware or software, so you can see what "automatic" chose. | Cast details now show which encoder is in use, and whether it is hardware. |
| Fixed a race condition in session teardown. | Starting a cast right after stopping one no longer fails. |
| Corrected the socket address family selection for mirroring. | Casts to devices that announce only IPv6 no longer fall back to slow HLS. |

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
