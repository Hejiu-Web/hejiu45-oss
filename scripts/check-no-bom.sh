#!/usr/bin/env bash
# Fail if any tracked text file starts with a UTF-8 BOM.
#
# Why: a BOM makes Cargo.toml unparseable for some tools (the rust-cache action
# rejected the manifest outright), and it is invisible in most editors. It crept
# in because Windows PowerShell 5.1 writes `Set-Content -Encoding utf8` *with*
# a BOM, unlike PowerShell 7.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

bad=0
while IFS= read -r f; do
  case "$f" in
    *.png|*.jpg|*.ico|*.gz|*.zip|*.exe) continue ;;
  esac
  # Read the first three bytes as hex.
  head3=$(head -c 3 "$f" | od -An -tx1 | tr -d ' \n')
  if [ "$head3" = "efbbbf" ]; then
    echo "BOM found: $f" >&2
    bad=1
  fi
done < <(git ls-files)

if [ "$bad" -ne 0 ]; then
  echo "" >&2
  echo "Remove the BOM, e.g.:" >&2
  echo "  sed -i '1s/^\xEF\xBB\xBF//' <file>    # or in PowerShell:" >&2
  echo "  [IO.File]::WriteAllText(\$p, (Get-Content \$p -Raw), (New-Object Text.UTF8Encoding \$false))" >&2
  exit 1
fi
echo "no BOMs found"
