---
name: translations
description: How to translate this extension's UI strings into any language so the wording matches the rest of the GNOME desktop - look the term up in GNOME's own catalogues (locally, or from l10n.gnome.org) instead of inventing one. Use when filling in a .po file, adding a language, reviewing a translation, or when new strings land in the .pot.
---

# Translating

The mechanics - `msginit`, `make translations`, where the `.mo` files go - are in
`extension/gnome-shell-cast@oxygenws.com/po/README.md`. This skill is about
**choosing the words**.

## The one rule

**Never invent wording for a term the desktop already has.** The user reads our
menu next to GNOME's own menus. If Settings calls it *Auflösung* and we call it
*Bildschirmgröße*, we look broken - even though both are "correct" German.

So for every non-obvious term: look up how GNOME translates it, and use that.
This holds for every language, including one nobody has added yet.

## Looking a term up

`gnome-term.sh` searches the GNOME catalogues installed on this machine:

```bash
.claude/skills/translations/gnome-term.sh de '^Resolution$'
.claude/skills/translations/gnome-term.sh fa 'quick settings'     # loose, for context
```

It takes any locale code (`de`, `de_DE`, `pt_BR`) and falls back to the base
code, since system catalogues are usually installed unsuffixed. Anchor the
pattern (`^Mute$`) for the term alone; leave it loose to see it used in a
sentence.

**Weigh the hits by module, do not just take the first line.** In descending
authority:

| Module | Why |
|---|---|
| `gnome-shell`, `mutter` | Same surface as ours - the panel, the menus |
| `gnome-control-center-2.0` | Where users set resolution, displays, sound |
| `libadwaita`, `gtk40`, `gsettings-desktop-schemas` | Toolkit-wide standard wording |
| `gstreamer-1.0`, `gst-plugins-*` | Codec, bitrate and stream terms |
| Application catalogues | Last resort, and only if the domain matches |

Two traps this ordering avoids:

- **The same English word has several right answers.** German *Volume* is
  `Lautstärke` in gnome-control-center, `Volumen` in gnome-calculator and
  `Datenträger` in gnome-disk-utility. Pick by domain, not by frequency.
- **Some shipped catalogues are junk.** The Persian `geany` catalogue is British
  English throughout. If a hit is not in the target language, or disagrees with
  every core module, drop it.

The script filters out fuzzy entries, which matters once you start downloading
`.po` files: a fuzzy hit is `msgmerge`'s guess from a *different* string, so
Arabic "Open the quick settings menu" comes back as "Open the application menu".
Installed `.mo` files never contain them, downloaded `.po` files are full of
them.

## A language with no catalogues installed

Then use [l10n.gnome.org](https://l10n.gnome.org/languages/), which hosts every
GNOME team's work.

1. **The team page**, `https://l10n.gnome.org/teams/<code>/` - the team's own
   website (that is where a glossary or style guide lives, if there is one), the
   coordinators, and the language's **plural forms**, which the `.po` header must
   declare correctly.
2. **One module**, straight to a file:
   ```bash
   curl -O https://l10n.gnome.org/POT/gnome-shell.gnome-50/gnome-shell.gnome-50.<code>.po
   ```
   Branch names are `gnome-NN` or `main`; check
   <https://l10n.gnome.org/languages/> for the current stable release.
3. **A whole release**, when you are translating the file end to end:
   ```bash
   curl -O https://l10n.gnome.org/languages/<code>/gnome-50/ui.tar.gz
   ```

Then search the downloads with the same script - it accepts extra `.po` files or
a directory of them:

```bash
.claude/skills/translations/gnome-term.sh it '^Resolution$' ./po-downloads/
```

## Rules that hold in every language

- **Match GNOME's register, not English's.** Most teams translate menu actions
  as infinitives or nouns rather than as commands aimed at the user. Copy
  whatever the core modules do; do not carry English imperative phrasing over.
- **Sentence case is a rule about the English source**, not about the
  translation. Capitalise by the target language's own rules - German nouns stay
  capitalised.
- **Placeholders survive intact.** `%s`, `%d` and the `%s` in `Version %s` must
  appear in the translation. If the sentence needs a different word order, use
  positional forms (`%1$s`, `%2$s`), never drop or reorder bare ones.
- **Do not translate names.** GNOME, Chromecast, Wayland, D-Bus, HLS, kbit/s,
  and the product name itself stay as they are.
- **Use the locale's own punctuation** - German `„…“`, French `« … »` with
  non-breaking spaces, Persian `«…»` and ZWNJ in compounds. Copy what the core
  modules do rather than reusing the English quotes.
- **Keep it short.** These strings sit in a panel menu and in `Adw.ActionRow`
  titles; a translation twice the length of the English ellipsises away.
- **No empty and no fuzzy entries.** A `#, fuzzy` marker means gettext shows the
  English instead, so it is not a placeholder you can leave behind.

## Finishing

```bash
make translations                       # extract → msgmerge → msgfmt
msgfmt -c --statistics extension/gnome-shell-cast@oxygenws.com/po/<lang>.po
```

`msgfmt -c` catches placeholder mismatches and a wrong plural-forms header.
Aim for `0 fuzzy, 0 untranslated`. To see the strings in place, run a nested
shell in the target language - it inherits the environment, so no logout and no
changing your own session:

```bash
LANG=de_DE.UTF-8 make run-nested
```

The locale has to be generated on the machine (`locale -a`) or gettext falls
back to English and the catalogue looks broken when it is fine.

When a term you had to research is likely to come up again, add it to the
glossary in `po/README.md` — the term and the module you took it from, not the
translation, so the entry still helps the next language.
