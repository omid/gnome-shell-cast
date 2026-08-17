# Translations for GNOME Shell Cast

This guide explains how to manage translations using gettext. For choosing the
*wording* — matching the terms the rest of the GNOME desktop already uses — see
the `translations` skill and `extension/gnome-shell-cast@oxygenws.com/po/README.md`.

## Quick Start

### Extract translatable strings from source code
```bash
make extract-translations
```

### Initialize a new language (creates .po file from .pot template)
```bash
msginit -i extension/gnome-shell-cast@oxygenws.com/po/gnome-shell-cast@oxygenws.com.pot \
        -o extension/gnome-shell-cast@oxygenws.com/po/it.po \
        -l it --no-translator
```

### Update existing translations when strings change
```bash
make update-translations
```

### Edit translations
Edit the `.po` files in the `extension/gnome-shell-cast@oxygenws.com/po/` directory using any text editor or a dedicated translation tool like Poedit:
```bash
extension/gnome-shell-cast@oxygenws.com/po/de.po
extension/gnome-shell-cast@oxygenws.com/po/fa.po
```

### Full translation workflow (extract → update)
```bash
make translations
```

Note: GNOME Shell automatically compiles `.po` files to `.mo` format when the extension is installed.

## Adding a new language

1. **Initialize the .po file:**
   ```bash
   msginit -i extension/gnome-shell-cast@oxygenws.com/po/gnome-shell-cast@oxygenws.com.pot \
           -o extension/gnome-shell-cast@oxygenws.com/po/fr_FR.po \
           -l fr_FR --no-translator
   ```

2. **Edit the .po file:**
   ```bash
   editor extension/gnome-shell-cast@oxygenws.com/po/fr_FR.po
   ```
   Add translations for each `msgid` string.

3. **Compile:**
   ```bash
   make compile-translations
   ```

4. **Test:**
   - Reinstall the extension: `make install-extension`
   - Change system language to French
   - Verify translations appear in the UI

## .PO File Format

Each entry looks like:
```
#: path/to/file.js:123
msgid "English string"
msgstr "Translated string"
```

- `#:` marks the source file and line number
- `msgid` is the original English text
- `msgstr` is the translated text

For strings with format placeholders:
```
msgid "Version %s"
msgstr "Versión %s"
```

Keep the `%s` placeholder in the same position!

## Current Languages

English is the source language and needs no `.po` file. Thirteen translations
ship: `ar`, `bg`, `bn`, `de`, `es`, `fa`, `fr`, `hi`, `id`, `pt_BR`, `ru`, `ur`,
`zh_CN` — all complete.

Files are named after the bare language code unless the regional variants
genuinely differ (`pt_BR`, `zh_CN`): gettext falls back from `de_AT` to `de`,
but never sideways to `de_DE`.

## Translation Tools

### Command-line tools
- `xgettext` - Extract strings from source code
- `msginit` - Initialize .po from .pot template
- `msgfmt` - Compile .po to .mo (binary)
- `msgmerge` - Update .po when strings change

### GUI tools
- [Poedit](https://poedit.net/) - Full-featured translation editor
- [Gtranslator](https://wiki.gnome.org/Apps/Gtranslator) - GNOME translation editor

## Files

- `extension/gnome-shell-cast@oxygenws.com/po/gnome-shell-cast@oxygenws.com.pot` - Translation template
- `extension/gnome-shell-cast@oxygenws.com/po/*.po` - Language files (de.po, fa.po, etc.)

## Makefile Targets

- `make extract-translations` - Extract translatable strings from source files
- `make update-translations` - Update .po files from .pot template (preserves existing translations)
- `make translations` - Full workflow: extract and update (GNOME Shell handles compilation)

## Tips

1. **Use the .pot file as reference** - Always check `gnome-shell-cast@oxygenws.com.pot` for the original strings
2. **Keep %s placeholders** - Format strings like `"Version %s"` must keep `%s` in translations
3. **Preserve whitespace** - Don't add/remove spaces in translations
4. **Test after translating** - Install and verify translations appear correctly
5. **Use translation memory** - Poedit and similar tools remember translations for reuse

## Contributing Translations

To contribute a new language:

1. Fork the repository
2. Create a new .po file:
   ```bash
   msginit -i extension/gnome-shell-cast@oxygenws.com/po/gnome-shell-cast@oxygenws.com.pot \
           -o extension/gnome-shell-cast@oxygenws.com/po/fr_FR.po \
           -l fr_FR --no-translator
   ```
3. Translate all strings in `extension/gnome-shell-cast@oxygenws.com/po/fr_FR.po`
4. Test: `make install-extension`, change system language, verify translations
5. Submit a pull request


Thank you for helping localize GNOME Shell Cast! 🌍
