#!/bin/sh
# All checks in one command:
#   tools/check.sh            the parts the branch touches (diff against origin/main, uncommitted and new files);
#   tools/check.sh --all      all parts;
#   tools/check.sh part...    only the named parts, with full output, stopping at the first error (how CI calls it);
#   tools/check.sh --update-snapshot   rewrite the render snapshot of an installation's parts and print the diff.
# Parts:
#   rust              cargo test in services;
#   rust-lint         cargo fmt --check and cargo clippy -D warnings;
#   web               hub-web: pnpm install, build, check, test;
#   node-tests        npm ci in .tools, then node --test tools/test-*.mjs;
#   chart             AppHost (npm ci, aspire restore), tools/chart.sh, deploy/chart without drift;
#   pg-schema         a no-op without services/hub/baseline: tools/canonical-bodies.sh --check, then, where docker is,
#                     tools/test-pg-schema.sh (the migration chain and the baseline give the same catalog,
#                     services/hub/pg-schema.sql, on a real Postgres). canonical-bodies.sh is trusted like the scripts
#                     below: next to this file if it is there, otherwise from origin/main of the checked repo, with
#                     --root set to the checked tree, so a change cannot weaken its own check;
#   cyrillic          tools/no-cyrillic.sh over the directories in cyrillic_dirs (hub-web/src, where only
#                     lib/i18n/ru.ts may have it, the hub code, AppHost, the chart and its overlay, the examples and
#                     services/hub/baseline); it needs grep -P with UTF-8;
#   factory-tree      the file list of `git archive HEAD` equals tools/factory-tree.txt (commit first: the archive
#                     is made from HEAD);
#   apphost           eslint and tsc --noEmit for apphost.mts;
#   helm-lint         helm lint of the chart with fake digests: with probe values for the required keys and with
#                     the installation values ($install_values), if any;
#   graph-pg          tools/test-graph-pg.sh: graph and routes on a real Postgres 18 in docker.
# Without arguments: one line per part; on error, the tail of its log. Logs live in a temporary directory that is
# removed. Locally mise provides the tool versions (the same as in the tools/Dockerfile image); without mise the tools
# come from PATH.
# CHECK_SH_ROOT: root of the repo under check, if the script does not live in it. CI can take this script from the
# default branch and check a pull request this way, so a change cannot weaken its own checks. The check scripts
# tools/chart.sh and tools/no-cyrillic.sh are taken from next to this file (the root above the tools/ it lives in),
# never from CHECK_SH_ROOT. The tests (tools/test-*.mjs, cargo test, hub-web tests) run from the checked tree: they
# test the changed code and change together with it.
# Installation parts: every tools/check.<name>.sh next to this file is sourced after the parts below. Such a file
# defines more part_* functions, lists them in install_parts, picks them by the diff in install_changed_parts and may
# set install_values, release and namespace. Never from CHECK_SH_ROOT.
# CHECK_RELEASE and CHECK_NAMESPACE: Helm release name and namespace for helm template.
set -eu
self="$(cd "$(dirname "$0")" && pwd)/$(basename "$0")"
trusted="$(cd "$(dirname "$self")/.." && pwd)"
for f in tools/chart.sh; do
  [ -f "$trusted/$f" ] || { echo "tools/check.sh: missing $trusted/$f (the gate puts it from main next to check.sh)" >&2; exit 2; }
done
# A second loop rather than a new word in the first one: a CI layout test from an older default branch may read only
# the first loop.
for f in tools/no-cyrillic.sh; do
  [ -f "$trusted/$f" ] || { echo "tools/check.sh: missing $trusted/$f (the gate puts it from main next to check.sh)" >&2; exit 2; }
done
cd "${CHECK_SH_ROOT:-$(dirname "$self")/..}"
CHECK_SH_ROOT="$(pwd)"
export CHECK_SH_ROOT
# One temporary directory per process; removed both on exit and on interrupt (Ctrl-C, step timeout).
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

# Versions as in tools/Dockerfile; rust takes services/rust-toolchain.toml via rustup.
if command -v mise >/dev/null 2>&1 && [ -z "${CHECK_SH_TOOLS:-}" ]; then
  rust_="$(sed -n 's/^channel = "\(.*\)"$/\1/p' services/rust-toolchain.toml)"
  tools_="rust@$rust_ node@26.13.1 pnpm@12.20.0 helm@4.3.0 yq@4.54.3 kubectl@1.34.13"
  # shellcheck disable=SC2086
  mise install -q $tools_ >/dev/null
  # shellcheck disable=SC2086
  PATH="$(mise bin-paths $tools_ | tr '\n' ':')$PATH"
  export PATH CHECK_SH_TOOLS=1
fi
export YQ="${YQ:-yq}"
release="${CHECK_RELEASE:-factory}"
namespace="${CHECK_NAMESPACE:-factory}"
# Values of the installation (helm flags): what Argo layers over the chart defaults. The installation parts file sets
# them; empty without it.
install_values=""
# Probe values for keys without a default (required in the templates): no made-up account default is needed.
probe_values="hub.instance.account=probe,hub.instance.timezone=UTC"
# Each clone and worktree has its own build directory (cargo default: services/target, removed with the worktree).
# A directory shared by several worktrees gave green parts on someone else's code: cargo checks freshness by the source
# paths of the previous build, those are the other worktree's paths, and it does not see an edit in this one.
# A build from scratch takes about 20 s. In the gate the cargo cache volume sets the directory.

all_parts="rust rust-lint web node-tests graph-pg chart apphost helm-lint pg-schema cyrillic factory-tree"
# Directories where Cyrillic is forbidden (cyrillic part). The baseline schema joins the list when it exists:
# no-cyrillic.sh refuses a missing directory.
cyrillic_dirs="hub-web/src services/hub/src apphost deploy/overlay deploy/chart examples"
[ ! -d services/hub/baseline ] || cyrillic_dirs="$cyrillic_dirs services/hub/baseline"

part_rust() {
  (cd services && cargo test --locked)
}

part_rust_lint() {
  (cd services && cargo fmt --check && cargo clippy -q --all-targets --locked -- -D warnings) || return 1
  # The chain replay tests (cfg `chain_tests`, services/hub/build.rs): clippy sees them, `cargo fmt` skips the module.
  [ ! -f services/hub/src/pg_tests_chain.rs ] || (cd services/hub && rustfmt --check --edition 2024 src/pg_tests_chain.rs)
}

part_web() {
  (cd hub-web && pnpm install --frozen-lockfile && pnpm run build && pnpm run check && pnpm run test)
}

part_node_tests() {
  npm ci --prefix .tools
  # One file at a time: PGlite in every test, and a CI step may share one memory limit. CHECK_SH_ROOT is passed as an
  # absolute path: it may be relative, and the script has already changed into that directory.
  CHECK_SH_ROOT="$PWD" node --test --test-concurrency=1 tools/test-*.mjs
}

# AppHost dependencies and Aspire modules (apphost/.aspire): without them there is neither a chart nor apphost.mts types.
aspire_restore_() {
  [ -d .tools/node_modules ] || npm ci --prefix .tools
  (cd apphost && npm ci)
  .tools/aspire restore --non-interactive
}

part_chart() {
  aspire_restore_
  # A copy before tools/chart.sh instead of git diff: a CI checkout may have no .git, and a new file that git diff
  # does not see is drift too.
  snap="$tmp/snap"
  mkdir "$snap"
  cp -a deploy/chart "$snap/chart"
  rc=0
  sh "$trusted/tools/chart.sh" || rc=$?
  if [ $rc = 0 ] && ! diff -ru "$snap/chart" deploy/chart; then
    echo "deploy/chart drifts from AppHost and overlay: run tools/chart.sh and commit the result" >&2
    rc=1
  fi
  return $rc
}

part_apphost() {
  aspire_restore_
  (cd apphost && npm run -s lint && npx tsc -p tsconfig.apphost.json --noEmit)
}

# The baseline schema services/hub/baseline against the migration chain. Without the baseline there is nothing to
# check. The stale check needs only sh and awk and runs in CI; the catalog comparison needs docker and runs locally.
part_pg_schema() {
  if [ ! -d services/hub/baseline ]; then
    echo "pg-schema: no services/hub/baseline, nothing to check"
    return 0
  fi
  # The trusted generator: next to check.sh (locally the same repo), else origin/main of the checked repo (CI may put
  # only a fixed list of files next to check.sh).
  gen="$trusted/tools/canonical-bodies.sh"
  if [ ! -f "$gen" ]; then
    gen="$tmp/canonical-bodies.sh"
    git -C "$CHECK_SH_ROOT" show origin/main:tools/canonical-bodies.sh >"$gen" || {
      echo "pg-schema: tools/canonical-bodies.sh is neither next to check.sh nor in origin/main" >&2
      return 1
    }
    echo "pg-schema: tools/canonical-bodies.sh from origin/main"
  fi
  sh "$gen" --root "$CHECK_SH_ROOT" --check
  if command -v docker >/dev/null 2>&1 && docker info >/dev/null 2>&1; then
    sh tools/test-pg-schema.sh
  else
    echo "pg-schema: no docker here, the catalog comparison (tools/test-pg-schema.sh) runs locally"
  fi
}

# The script next to this file (from main in the gate), directories and files from the checked tree: CHECK_SH_ROOT.
# shellcheck disable=SC2086
part_cyrillic() { sh "$trusted/tools/no-cyrillic.sh" $cyrillic_dirs; }

# The published tree is `git archive HEAD` (.gitattributes decides what is in it); every file of it is a line of
# tools/factory-tree.txt, so a new published file shows in the diff of the change that adds it.
part_factory_tree() {
  [ -f tools/factory-tree.txt ] || { echo "missing tools/factory-tree.txt" >&2; return 1; }
  git archive HEAD | tar -t | grep -v '/$' | LC_ALL=C sort | diff -u tools/factory-tree.txt - \
    || { echo "tools/factory-tree.txt differs from git archive HEAD (lines with + are in the archive)" >&2; return 1; }
}

part_helm_lint() {
  helm lint deploy/chart -f tools/values.fake-digests.yaml --set "$probe_values"
  # shellcheck disable=SC2086
  [ -z "$install_values" ] || helm lint deploy/chart -f tools/values.fake-digests.yaml $install_values
}

part_graph_pg() { sh tools/test-graph-pg.sh; }

# Installation parts (see the header). install_parts is read by the gate layout test as well.
install_parts=""
for f in "$trusted"/tools/check.*.sh; do
  [ ! -f "$f" ] || . "$f"
done
[ -z "$install_parts" ] || all_parts="$all_parts $install_parts"

run_part() { "part_$(echo "$1" | sed "s/-/_/g")"; }

if [ "${1:-}" = "--update-snapshot" ]; then
  [ $# = 1 ] || { echo "tools/check.sh: --update-snapshot takes no other arguments" >&2; exit 2; }
  command -v part_render_equiv >/dev/null 2>&1 || { echo "tools/check.sh: no render-equiv part (installation parts file)" >&2; exit 2; }
  CHECK_UPDATE_SNAPSHOT=1 run_part render-equiv
  exit 0
fi

# Explicitly named parts: this is how the gate steps call them. Full output, the first error stops.
if [ $# -gt 0 ] && [ "$1" != "--all" ]; then
  for p in "$@"; do
    case " $all_parts " in *" $p "*) ;; *) echo "tools/check.sh: no part $p (available: $all_parts)" >&2; exit 2 ;; esac
  done
  for p in "$@"; do
    echo "== $p"
    run_part "$p"
  done
  exit 0
fi

if [ "${1:-}" = "--all" ]; then
  parts="$all_parts"
else
  # Without origin/main the diff is empty, and the branch would come out green without checking anything.
  git rev-parse -q --verify origin/main >/dev/null || { echo "tools/check.sh: no origin/main, run git fetch origin" >&2; exit 2; }
  git merge-base origin/main HEAD >/dev/null || { echo "tools/check.sh: HEAD and origin/main share no history" >&2; exit 2; }
  changed="$( (git diff --name-only origin/main...HEAD; git diff --name-only HEAD; git ls-files --others --exclude-standard) | sort -u)"
  has() { echo "$changed" | grep -qE "$1"; }
  parts=""
  # A change to the check itself or to the gate is checked in full.
  if has '^(tools/check(\.[a-z]+)?\.sh|\.tekton/)'; then
    parts="$all_parts"
  else
    has '^services/' && parts="$parts rust rust-lint"
    has '^hub-web/' && parts="$parts web"
    # The tools/ tests read hub-web code.
    has '^(services|hub-web|tools|\.tools)/' && parts="$parts node-tests"
    # A real Postgres: migrations, the link seed, code that talks to the graph database, and the Better Auth config with its version.
    has '^(services/hub/migrations/|services/hub/baseline/|services/hub/(Cargo\.toml|build\.rs)$|services/hub/seed/|services/hub/src/(adopt|migrations|main|pg_tests|pg_tests_chain|graph_view|graph|people|query|documents|callers|plan|plan_seed|logins|mailbox|tx)\.rs$|hub-web/src/lib/server/(auth-config|auth-pg\.test|tailnet-auth)\.ts$|hub-web/(auth\.cli\.ts|package\.json)$|tools/test-graph-pg\.sh$)' && parts="$parts graph-pg"
    has '^(apphost|deploy|tools|\.tools)/|^aspire\.config\.json$|/Dockerfile$' && parts="$parts apphost chart helm-lint"
    has '^(services/hub/(migrations|baseline)/|tools/(canonical-bodies\.sh|test-pg-schema\.sh|pg-schema\.sql|test-graph-pg\.sh)$)' && parts="$parts pg-schema"
    # A change to tools/no-cyrillic.sh itself too: otherwise a weakened script would pass unchecked.
    for d in $cyrillic_dirs services/hub/baseline tools/no-cyrillic.sh; do
      if has "^${d%/}(/|\$)"; then parts="$parts cyrillic"; break; fi
    done
    # Installation parts pick themselves by the same diff (the installation parts file).
    ! command -v install_changed_parts >/dev/null 2>&1 || parts="$parts $(install_changed_parts)"
  fi
  [ -n "$parts" ] || { echo "nothing to check: the branch does not touch services/, hub-web/, apphost/, deploy/, tools/ (--all checks everything)"; exit 0; }
fi

log="$tmp/logs"
mkdir "$log"
fail=0
for p in $parts; do
  start=$(date +%s)
  # A separate process rather than a function under if: set -e does not apply inside a condition, and an error in the
  # middle of a part would be lost.
  if sh "$self" "$p" >"$log/$p.log" 2>&1; then
    echo "ok   $p ($(( $(date +%s) - start )) s)"
  else
    echo "FAIL $p ($(( $(date +%s) - start )) s):"
    tail -n 30 "$log/$p.log" | sed 's/^/     /'
    fail=1
  fi
done
exit $fail
