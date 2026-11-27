#!/bin/sh
# Canonical bodies (steps 2.12b and later of the factory plan): every function, procedure and view of the database
# has exactly the text of the baseline schema, and every comment of the baseline is set, from migration 0028 on.
#   sh tools/canonical-bodies.sh           print the statements a new migration needs: every function, view and
#                                          comment of the baseline that the canonical migrations do not reproduce yet
#                                          (missing, or with another text), as CREATE OR REPLACE / COMMENT ON
#   sh tools/canonical-bodies.sh --check   exit 1 with the differences when that list is not empty
#   sh tools/canonical-bodies.sh --stdout  print every function, view and comment of the baseline (what 0028 has)
#   --root DIR (before the mode): the repo to read, by default the one this script lives in. The pg-schema part of
#   tools/check.sh runs the copy from main with --root set to the checked tree, so a PR cannot weaken the check; a
#   change to this script lands in an earlier PR (lesson L26).
# Input: services/hub/baseline/1000_baseline.sql and the canonical migrations: services/hub/migrations/NNNN_*.sql with
# NNNN >= 0028 (0028_canonical_bodies.sql gave every object its baseline text; it is frozen like every applied
# migration, sqlx keeps its checksum). Each file is split into statements at top-level semicolons (quotes, quoted
# identifiers, dollar quotes and comments are skipped); every CREATE [OR REPLACE] FUNCTION, PROCEDURE and VIEW
# becomes CREATE OR REPLACE with the same text, every COMMENT ON is kept, other statements are left out. A kind that
# CREATE OR REPLACE cannot carry over (materialized view, aggregate, recursive or temporary view) is refused.
# Each statement has a key: the function or procedure with its argument list, the view name, the COMMENT ON target
# (blanks collapsed). The canonical migrations are read in order and the last statement of a key wins; a DROP
# FUNCTION, PROCEDURE or VIEW forgets every key of that name (all overloads) and the comments on it. The check: for every statement of the baseline the
# last statement of its key equals it byte for byte, and no key of the canonical migrations is missing from the
# baseline. So a migration that changes a function copies the new baseline text of it verbatim (the default mode
# prints it). The catalog itself (tables, constraints, grants, ...) is compared by tools/test-pg-schema.sh.
set -eu
root="$(dirname "$0")/.."
if [ "${1:-}" = --root ]; then
  [ -n "${2:-}" ] || { echo "tools/canonical-bodies.sh: --root needs a directory" >&2; exit 2; }
  root="$2"
  shift 2
fi
cd "$root"
baseline=services/hub/baseline/1000_baseline.sql
first=28
[ -f "$baseline" ] || { echo "tools/canonical-bodies.sh: no $baseline" >&2; exit 2; }

# Canonical statements of one file, each followed by a line with a single \036 (record separator). DROP FUNCTION,
# PROCEDURE and VIEW come out as one "DROP <name>" record per dropped name.
statements() {
  awk '
    { s = s $0 "\n" }
    END {
      # One pass, one character at a time: st is "" (code), "q" (quote), "e" (E'' quote with backslash escapes),
      # "d" (quoted identifier), "l" (line comment),
      # "b" (block comment) or "$" (dollar quote with tag tag).
      n = length(s); start = 1; nst = 0; st = ""
      for (i = 1; i <= n; i++) {
        c = substr(s, i, 1)
        if (st == "") {
          if (c == "'\''") st = (substr(s, i - 1, 1) ~ /[Ee]/ && substr(s, i - 2, 1) !~ /[A-Za-z0-9_]/) ? "e" : "q"
          else if (c == "\"") st = "d"
          else if (c == "-" && substr(s, i + 1, 1) == "-") st = "l"
          else if (c == "/" && substr(s, i + 1, 1) == "*") { st = "b"; i++ }
          else if (c == "$" && match(substr(s, i, 80), /^\$([A-Za-z_][A-Za-z_0-9]*)?\$/)) { st = "$"; tag = substr(s, i, RLENGTH); i += RLENGTH - 1 }
          else if (c == ";") { stmt[++nst] = substr(s, start, i - start + 1); start = i + 1 }
        } else if (st == "q") { if (c == "'\''") st = "" }
        else if (st == "e") { if (c == "\\") i++; else if (c == "'\''") st = "" }
        else if (st == "d") { if (c == "\"") st = "" }
        else if (st == "l") { if (c == "\n") st = "" }
        else if (st == "b") { if (c == "*" && substr(s, i + 1, 1) == "/") { st = ""; i++ } }
        else if (c == "$" && substr(s, i, length(tag)) == tag) { st = ""; i += length(tag) - 1 }
      }
      if (st != "" && st != "l") fail("unterminated " (st == "$" ? tag : st))
      if (strip(substr(s, start)) != "") fail("text after the last statement")
      for (k = 1; k <= nst; k++) {
        t = strip(stmt[k]); u = toupper(substr(t, 1, 80))
        if (u ~ /^CREATE[ \t\n]+(OR[ \t\n]+REPLACE[ \t\n]+)?(MATERIALIZED|RECURSIVE|TEMP|TEMPORARY)[ \t\n]+VIEW/ || u ~ /^CREATE[ \t\n]+(OR[ \t\n]+REPLACE[ \t\n]+)?AGGREGATE/)
          fail("unsupported statement: " substr(t, 1, 60))
        if (match(u, /^CREATE[ \t\n]+(OR[ \t\n]+REPLACE[ \t\n]+)?(FUNCTION|PROCEDURE|VIEW)[ \t\n]/)) {
          match(u, /^CREATE[ \t\n]+(OR[ \t\n]+REPLACE[ \t\n]+)?/)
          print "CREATE OR REPLACE " substr(t, RLENGTH + 1); print "\036"
        } else if (u ~ /^COMMENT[ \t\n]+ON[ \t\n]/) {
          print t; print "\036"
        } else if (match(u, /^DROP[ \t\n]+(FUNCTION|PROCEDURE|VIEW)[ \t\n]+(IF[ \t\n]+EXISTS[ \t\n]+)?/)) {
          # DROP FUNCTION a(x, y), b: one record per name, split at the commas outside the parentheses.
          r = substr(t, RLENGTH + 1); d = 0; p = 1
          for (j = 1; j <= length(r) + 1; j++) {
            c = substr(r, j, 1)
            if (c == "(") d++
            else if (c == ")") d--
            else if ((c == "," && d == 0) || j > length(r)) {
              x = substr(r, p, j - p); sub(/^[ \t\n]+/, "", x); sub(/[ \t\n(;].*$/, "", x)
              if (x != "") { print "DROP " x; print "\036" }
              p = j + 1
            }
          }
        }
      }
    }
    # Leading blanks and comments of a statement are not part of it.
    function strip(t) {
      while (1) {
        sub(/^[ \t\n]+/, "", t)
        if (substr(t, 1, 2) == "--") { j = index(t, "\n"); if (!j) return ""; t = substr(t, j + 1); continue }
        if (substr(t, 1, 2) == "/*") { j = index(t, "*/"); t = substr(t, j + 2); continue }
        return t
      }
    }
    function fail(m) { print "tools/canonical-bodies.sh: " FILENAME ": " m > "/dev/stderr"; exit 2 }
  ' "$1"
}

# The canonical migrations in order, by version.
migrations() {
  for f in services/hub/migrations/[0-9]*_*.sql; do
    [ -f "$f" ] || continue
    v="$(basename "$f" | sed 's/_.*//; s/^0*//')"
    [ "${v:-0}" -ge "$first" ] && echo "$f"
  done
}

# Input: the baseline statements, a line "\035", then the statements of the canonical migrations. Mode "missing"
# prints the baseline statements that the migrations do not reproduce; mode "check" prints the differences to stderr.
compare() { # mode
  awk -v mode="$1" '
    BEGIN { side = 0; t = "" }
    $0 != "\036" { t = (t == "" ? $0 : t "\n" $0); next }
    {
      if (t == "\035") { side = 1; t = ""; next }
      if (t ~ /^DROP /) { name = substr(t, 6); for (k in mig) if (namek(k) == name || index(namek(k), name ".") == 1) delete mig[k]; t = ""; next }
      k = key(t)
      if (side == 0) { if (k in base) fail("two statements in the baseline for " k); base[k] = t; order[++n] = k }
      else mig[k] = t
      t = ""
    }
    END {
      bad = 0
      for (i = 1; i <= n; i++) {
        k = order[i]
        if ((k in mig) && mig[k] == base[k]) continue
        bad++
        if (mode == "missing") { print "\n" base[k]; continue }
        print ((k in mig) ? "differs from the baseline: " : "not in the canonical migrations: ") k > "/dev/stderr"
      }
      for (k in mig) if (!(k in base)) {
        bad++
        if (mode == "check") print "not in the baseline: " k > "/dev/stderr"
        else print "tools/canonical-bodies.sh: not in the baseline: " k > "/dev/stderr"
      }
      if (mode == "check" && bad) exit 1
    }
    # FUNCTION name(args) or PROCEDURE name(args) up to the parenthesis that closes the arguments, VIEW name, or the
    # COMMENT ON target; blanks collapsed.
    function key(t,   u, i, d, c, q, r) {
      if (t ~ /^COMMENT/) { i = index(t, " IS "); if (!i) i = index(t, "\nIS "); r = substr(t, 1, i - 1) }
      else if (t ~ /^CREATE OR REPLACE VIEW/) { r = substr(t, 24); sub(/[ \t\n(].*$/, "", r); r = "VIEW " r }
      else {
        u = substr(t, 19); d = 0; q = 0
        for (i = 1; i <= length(u); i++) {
          c = substr(u, i, 1)
          if (q) { if (c == "'\''") q = 0; continue }
          if (c == "'\''") q = 1
          else if (c == "(") d++
          else if (c == ")") { d--; if (d == 0) break }
        }
        r = substr(u, 1, i)
      }
      gsub(/[ \t\n]+/, " ", r)
      return r
    }
    # The bare name of a key: FUNCTION a.b(...) -> a.b, COMMENT ON COLUMN a.b.c -> a.b.c (a DROP of a.b forgets it too).
    function namek(k,   r) { r = k; sub(/^COMMENT ON /, "", r); sub(/^[A-Z]+ /, "", r); sub(/[ (].*$/, "", r); return r }
    function fail(m) { print "tools/canonical-bodies.sh: " m > "/dev/stderr"; exit 2 }
  '
}

input() {
  statements "$baseline"
  printf '\035\n\036\n'
  for f in $(migrations); do statements "$f"; done
}

case "${1:-}" in
  --stdout)
    statements "$baseline" | awk '$0 != "\036" { t = (t == "" ? $0 : t "\n" $0); next } { print "\n" t; t = "" }'
    ;;
  --check)
    if ! input | compare check; then
      echo "tools/canonical-bodies.sh: the canonical migrations (from 00$first) do not reproduce the functions, views and comments of $baseline: add a migration with the output of sh tools/canonical-bodies.sh" >&2
      exit 1
    fi
    echo "the canonical migrations (from 00$first) reproduce the functions, views and comments of $baseline"
    ;;
  "") input | compare missing ;;
  *) echo "usage: sh tools/canonical-bodies.sh [--root DIR] [--check|--stdout]" >&2; exit 2 ;;
esac
