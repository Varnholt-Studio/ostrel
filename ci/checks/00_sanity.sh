#!/usr/bin/env bash
# Grundhygiene: Konfliktmarker, Riesendateien, Geheimnisse, kaputte Shell-Skripte.
set -uo pipefail
bad=0
lt=$(printf '<%.0s' 1 2 3 4 5 6 7); gt=$(printf '>%.0s' 1 2 3 4 5 6 7)
if grep -rInE --exclude-dir=.git "^(${lt}|${gt})( |$)" . ; then echo "Merge-Konfliktmarker gefunden"; bad=1; fi
big=$(find . -path ./.git -prune -o -type f -size +5M -print)
if [ -n "$big" ]; then echo "Dateien > 5 MB: $big"; bad=1; fi
key="PRIVATE"" KEY-----"; aws="AKIA""[0-9A-Z]{16}"
if grep -rIlE --exclude-dir=.git -e "$key" -e "$aws" . ; then echo "Moegliches Geheimnis im Repo"; bad=1; fi
while IFS= read -r f; do bash -n "$f" || { echo "Syntaxfehler: $f"; bad=1; }; done < <(find . -path ./.git -prune -o -name '*.sh' -print)
exit $bad
