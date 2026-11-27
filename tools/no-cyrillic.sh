#!/bin/sh
# Cyrillic in directories already translated to English (step 2.1 of the factory plan, cyrillic part of tools/check.sh):
#   tools/no-cyrillic.sh directory...
# Checks the content and names of files under git in the named directories. There is one exception, exactly
# hub-web/src/lib/i18n/ru.ts (Russian UI strings; its name is checked). Without directories there is nothing to check, exit code 0.
# Found: a list of files and exit code 1. Without .git (the repo copy in .tekton/main.yaml) find lists the files.
set -eu
cd "${CHECK_SH_ROOT:-$(dirname "$0")/..}"
[ $# -gt 0 ] || { echo "no-cyrillic: no directories, nothing to check"; exit 0; }
# Otherwise a typo or a rename in the directory list would silently check nothing.
for d; do [ -e "$d" ] || { echo "no-cyrillic: no directory $d" >&2; exit 2; }; done
# grep -P understands \x{...} only in UTF-8.
LC_ALL=C.UTF-8
export LC_ALL
cyr='[\x{0400}-\x{04FF}]'
# grep errors below are swallowed (exit code 1 for "not found" through xargs cannot be told apart from them), so probe
# first: without it a grep without -P (busybox in alpine/k8s) or without UTF-8 reported "clean" on any tree.
if ! printf '\320\266\n' | grep -qP "$cyr" 2>/dev/null || printf 'z\n' | grep -qP "$cyr" 2>/dev/null; then
  echo "no-cyrillic: needs grep -P with UTF-8 (GNU grep, locale C.UTF-8), this grep has neither" >&2
  exit 2
fi
allowed=hub-web/src/lib/i18n/ru.ts
list="$(mktemp)"
trap 'rm -f "$list"' EXIT
if git rev-parse --is-inside-work-tree >/dev/null 2>&1; then
  git ls-files -z -- "$@" | tr '\0' '\n' >"$list"
else
  # Like git ls-files: installed dependencies (node_modules, ignored by git) are not ours.
  find "$@" -name node_modules -prune -o -type f -print | sed 's|^\./||' >"$list"
fi
bad="$(
  grep -P "$cyr" "$list" | sed 's/$/ (name)/' || true
  grep -vxF "$allowed" "$list" | tr '\n' '\0' | xargs -0 -r grep -lP "$cyr" -- || true
)"
if [ -n "$bad" ]; then
  echo "Cyrillic in files:" >&2
  echo "$bad" | sed 's/^/  /' >&2
  exit 1
fi
echo "no-cyrillic: clean ($(grep -c . "$list" || true) files)"
