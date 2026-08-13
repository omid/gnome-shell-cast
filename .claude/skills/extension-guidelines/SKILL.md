---
name: extension-guidelines
description: The upstream rules GNOME Shell extension code is held to - GJS best practices, the extensions.gnome.org review guidelines, and the EGO AI reference. Read these before writing or reviewing anything under extension/, before answering "does this match the guidelines?", and before preparing a release for extensions.gnome.org.
---

# Extension guidelines

Three upstream pages govern this extension. Fetch and read the relevant one
rather than answering from memory - the shipped shell sources tell you what the
API *is*, not what reviewers require.

| Page | Covers |
|---|---|
| <https://gjs.guide/extensions/review-guidelines/best-practices.html> | How extension code should be written: cleanup, signal handling, timeouts, session modes, GObject use |
| <https://gjs.guide/extensions/review-guidelines/review-guidelines.html> | The rules extensions.gnome.org reviewers apply; rejection reasons |
| <https://blogs.gnome.org/jrahmatzadeh/2026/07/27/ego-ai-reference/> | The EGO AI reference - what AI-assisted extension work is expected to get right |

Also useful: <https://gjs.guide/extensions/development/creating.html>

## Checking compliance locally

```sh
make shexli   # validates the packaged extension against the EGO review rules
make eslint   # project rules (complexity, correctness, no var, ...)
npx prettier --check extension/   # separate from eslint; check-all runs both
```

`shexli` should stay at **0 errors / 0 warnings**. It currently reports one
`manual_review` finding for `St.Clipboard.get_default()` in `errorDialog.js` and
`setupDialog.js` - that is a "a human should look at this" flag, not a defect.

## What an EGO reviewer has asked for on this extension

Real review feedback, generalised. Apply these before submitting rather than
waiting to be told again.

- **Construct nothing outside the class that owns it.** Module scope and
  initialization are for static data only; anything created has to be built by
  the class that will destroy it. See
  [initialization](https://gjs.guide/extensions/review-guidelines/review-guidelines.html#only-use-initialization-for-static-resources)
  and [destroy all objects](https://gjs.guide/extensions/review-guidelines/review-guidelines.html#destroy-all-objects).
- **Prefer `connectObject()` / `disconnectObject()`** over `connect()` with a
  hand-kept handler id. One `disconnectObject(this)` cannot leave a handler
  behind, and a reviewer can verify cleanup at a glance.
- **Keep sources plain ASCII unless a character earns its place.** Typographic
  punctuation invites mojibake, and in a translatable string it silently edits
  the msgid.
- **`constructor()`, never `_init()`.**

## Rules this project has already been bitten by

- **Public shell API only.** No `_private` members of shell objects. Where the
  only route is private, guard it so a throw cannot abort `enable()` and cost
  the user the whole extension.
- **Everything created in `enable()` dies in `disable()`** - widgets, signal
  handlers, `GLib` sources, and anything parented outside your own actor (a
  label added to `Main.layoutManager.uiGroup` outlives the menu).
- **`prefs.js` runs in a separate GTK process.** Importing `Main`, `St`,
  `Clutter` or `Shell` there is a rejection.
- **User-visible strings go through `_()`**, then `make translations`, then fill
  in `de_DE.po` and `fa_IR.po`. Watch apostrophes: a `'` vs `’` change edits the
  msgid and silently fuzzies the entry, which drops it at compile time.
- **`gnome-extensions disable/enable` does not reload changed JS** - the shell
  caches ES modules per process. See the `install-verify` skill.
