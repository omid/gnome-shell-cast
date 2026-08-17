#!/usr/bin/env bash
# Show how GNOME already translates a term in a given language.
#
#   ./gnome-term.sh <lang> <pattern> [extra .po/.mo files or dirs...]
#
# <lang>    locale code - "de", "de_DE", "pt_BR" (falls back to the base code)
# <pattern> case-insensitive regex matched against the English msgid;
#           anchor it (^Preferences$) for the term alone, leave it loose
#           (screencast) to see the term in context.
#
# Extra arguments let you search catalogues that are not installed system-wide,
# e.g. ones downloaded from l10n.gnome.org.
set -uo pipefail

[ $# -ge 2 ] || { sed -n '2,12p' "$0" | sed 's/^# \?//'; exit 2; }

lang=$1 pat=$2
shift 2

dir=/usr/share/locale/$lang/LC_MESSAGES
[ -d "$dir" ] || dir=/usr/share/locale/${lang%%_*}/LC_MESSAGES

catalogues=()
[ -d "$dir" ] && catalogues+=("$dir"/*.mo)
for extra in "$@"; do
    if [ -d "$extra" ]; then
        catalogues+=("$extra"/*.po "$extra"/*.mo)
    else
        catalogues+=("$extra")
    fi
done

if [ ${#catalogues[@]} -eq 0 ]; then
    echo "No catalogues for '$lang'. Install the language, or download it:" >&2
    echo "  curl -O https://l10n.gnome.org/POT/gnome-shell.gnome-49/gnome-shell.gnome-49.$lang.po" >&2
    exit 1
fi

found=0
for cat in "${catalogues[@]}"; do
    [ -f "$cat" ] || continue
    name=$(basename "$cat"); name=${name%.mo}; name=${name%.po}
    case $cat in
        *.mo) text=$(msgunfmt "$cat" 2>/dev/null) ;;
        *) text=$(cat "$cat") ;;
    esac
    hits=$(printf '%s\n' "$text" |
        msgattrib --translated --no-fuzzy --no-obsolete - 2>/dev/null |
        msggrep --msgid -i -e "$pat" - 2>/dev/null |
        msgcat --no-wrap - 2>/dev/null |
        awk -v m="$name" '
            /^msgid "/  { id = substr($0, 8, length($0) - 8) }
            /^msgstr "/ { s = substr($0, 9, length($0) - 9)
                          if (id != "" && s != "")
                              printf "%-26s | %-44s | %s\n", m, id, s }')
    [ -n "$hits" ] && { printf '%s\n' "$hits"; found=1; }
done

[ $found -eq 1 ] || { echo "No GNOME translation of /$pat/ in $lang." >&2; exit 1; }
