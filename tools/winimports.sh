#!/usr/bin/env bash
# Lists what a Windows program or DLL imports, one line per DLL.
#
#   tools/winimports.sh program.exe
set -euo pipefail
x86_64-w64-mingw32-objdump -p "$1" | awk '
    /DLL Name:/ { if (dll) print dll ":" names; dll = $3; names = ""; next }
    dll && /^\t[0-9a-f]+\t +[0-9]+ / { names = names " " $3 }
    END { if (dll) print dll ":" names }
'
