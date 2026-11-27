#!/bin/sh
# Graph cycle check with two parallel connections on a real Postgres 18 (PGlite has one
# connection, tools/test-graph.mjs does not cover this). Only on a machine with docker; the gate has no docker.
#   sh tools/test-graph-pg.sh
# Starts a temporary postgres:18 container (pinned by digest), applies services/hub/migrations in order and checks:
#   1. T1 inserts a blocks b and holds the transaction; T2 inserts b blocks a in parallel and waits for the lock;
#      after T1 commits, T2's insert fails with GRCYC and path ["b", "a", "b"];
#   2. the same in REPEATABLE READ: an insert with a cycle check is refused at once (GRISO);
#   3. TRUNCATE of every table that forbids DELETE (and with CASCADE) is refused (GRFRB);
#   4. ticket fields, link seed and /api/graph, end-to-end scope plan computation,
#      scope seed and its plan, skipping deferred scopes in the queue, document sync with the caller bound to a repo and the sqlx migration order,
#      schema verification at service start, login binding and mailbox keys, the instance row (step 2.3), the installation time zone in plan and queue (step 2.5), the plan input hash across one migration of the chain (step 2.4, only with pg_tests_chain.rs) through the hub routes: services/hub/src/pg_tests.rs (cargo test --ignored) on a separate database hub_rs
#      of the same container, port on 127.0.0.1 only. The queries of these tests run as role hub, a separate
#      test checks the boundaries of roles hub and hub_web;
#   4b. the same tests with HUB_MIGRATIONS=baseline (steps 2.12c, 2.15) on database hub_rs_base: the baseline
#      applies the normal way, the routes work on it, and `hub migrate --adopt` (src/adopt.rs) adopts a database built by
#      the chain and refuses a foreign or failed journal row, an extra or missing version, an extra column and a changed
#      function comment;
#   5. Better Auth of hub-web as role hub_web: hub-web/src/lib/server/auth-pg.test.ts on database hub;
#   6. Better Auth generator: auth generate with hub-web/auth.cli.ts of the same version as better-auth in
#      hub-web/package.json answers "already up to date" on database hub and generates tables on the empty database
#      postgres. The CLI config loader must understand the #lib/* import from auth-config.ts. The auth package comes from
#      pnpm dlx (network on the first run, the pnpm cache after that). The baseline database hub_rs_base is up to date too.
# The container is removed on any exit.
set -eu
cd "$(dirname "$0")/.."
name="hub-graph-pg-$$"
# Pinned by digest (multi-arch index of postgres:18.7); the tag is for reading only.
image=docker.io/library/postgres:18.7@sha256:c699303802144847d238196173655dbbecf7a96aeca1565f2b02eb671a1f0b6b
trap 'docker rm -f "$name" >/dev/null 2>&1 || true' EXIT INT TERM
docker run -d --rm --name "$name" -p 127.0.0.1::5432 -e POSTGRES_PASSWORD=x -e POSTGRES_DB=hub "$image" >/dev/null
i=0
until docker exec "$name" pg_isready -q -U postgres -d hub 2>/dev/null; do
  i=$((i + 1))
  [ "$i" -lt 60 ] || { echo "postgres did not start" >&2; exit 1; }
  sleep 1
done
psql_() { docker exec -i "$name" psql -X -q -v ON_ERROR_STOP=1 -U postgres -d hub "$@"; }
# pg_isready answers during initdb as well: wait for a real query.
until psql_ -c 'select 1' >/dev/null 2>&1; do sleep 1; done
for f in services/hub/migrations/*.sql; do
  { echo 'BEGIN;'; cat "$f"; echo 'COMMIT;'; } | psql_ >/dev/null
done
# Like Job hub-db-migrate: the instance row (0023) after the migrations; without it the account default (0025) gives HUBIN.
echo "SELECT public.seed_instance('demo', 'Europe/Athens', 20, NULL, ARRAY['docs/superpowers']);" | psql_ >/dev/null
psql_ <<'SQL' >/dev/null
BEGIN;
SELECT set_config('hub.actor', 'seed:test', true), set_config('hub.actor_kind', 'seed', true);
INSERT INTO tickets(project, code, summary) VALUES ('apps', 'a', 'a'), ('apps', 'b', 'b');
COMMIT;
SQL

edge() { # src dst: insert a blocks edge between tickets by code
  echo "INSERT INTO graph.edge(src, src_kind, rel, dst, dst_kind)
    SELECT s.node_id, 'ticket', 'blocks', d.node_id, 'ticket'
    FROM graph.ticket s JOIN tickets st ON st.seq = s.ticket_seq,
         graph.ticket d JOIN tickets dt ON dt.seq = d.ticket_seq
    WHERE st.code = '$1' AND dt.code = '$2';"
}
actor="SELECT set_config('hub.actor', 'user:test', true), set_config('hub.actor_kind', 'human', true);"
out1="$(mktemp)"
out2="$(mktemp)"
trap 'docker rm -f "$name" >/dev/null 2>&1 || true; rm -f "$out1" "$out2"' EXIT INT TERM

# 1. Two parallel inserts: the second waits for the lock and sees the first one's commit.
{ echo "BEGIN; $actor"; edge a b; echo "SELECT pg_sleep(3); COMMIT;"; } | psql_ >"$out1" 2>&1 &
t1=$!
sleep 1
{ echo "BEGIN; $actor"; edge b a; echo "COMMIT;"; } | psql_ -v VERBOSITY=verbose >"$out2" 2>&1 || true
wait "$t1"
grep -q 'GRCYC' "$out2" || { echo "FAIL: parallel cycle was not refused" >&2; cat "$out2" >&2; exit 1; }
grep -q '\["b", "a", "b"\]' "$out2" || { echo "FAIL: cycle path" >&2; cat "$out2" >&2; exit 1; }
n="$(echo "SELECT count(*) FROM graph.edge WHERE valid_to IS NULL;" | psql_ -t -A)"
[ "$n" = 1 ] || { echo "FAIL: live edges $n, want 1" >&2; exit 1; }
echo "ok: parallel b->a waited for a->b and was refused with GRCYC [b, a, b]"

# 2. REPEATABLE READ: the snapshot is older than the other commit, so the cycle check is forbidden in it.
{ echo "BEGIN ISOLATION LEVEL REPEATABLE READ; $actor"; edge b a; echo "COMMIT;"; } \
  | psql_ -v VERBOSITY=verbose >"$out2" 2>&1 || true
grep -q 'GRISO' "$out2" || { echo "FAIL: REPEATABLE READ was not refused" >&2; cat "$out2" >&2; exit 1; }
echo "ok: REPEATABLE READ refused with GRISO"

# 3. TRUNCATE on a real Postgres: the table list comes from the forbid_delete triggers.
tables="$(echo "SELECT n.nspname || '.' || c.relname FROM pg_trigger g JOIN pg_class c ON c.oid = g.tgrelid
  JOIN pg_namespace n ON n.oid = c.relnamespace WHERE g.tgname = 'forbid_delete' ORDER BY 1;" | psql_ -t -A)"
[ -n "$tables" ] || { echo "FAIL: no forbid_delete triggers" >&2; exit 1; }
for t in $tables; do
  echo "TRUNCATE $t CASCADE;" | psql_ -v VERBOSITY=verbose >"$out2" 2>&1 && {
    echo "FAIL: TRUNCATE $t passed" >&2; exit 1; }
  grep -q 'GRFRB' "$out2" || { echo "FAIL: TRUNCATE $t" >&2; cat "$out2" >&2; exit 1; }
done
echo "ok: TRUNCATE refused on $(echo "$tables" | wc -l) tables"

# 4. Ticket fields: hub logic (full set, repeat, 404, 403, create rollback, retired nodes) and two parallel
# edits over two connections. The test installs the migrations and seeds itself.
echo "CREATE DATABASE hub_rs;" | psql_ >/dev/null
port="$(docker port "$name" 5432/tcp | sed -n 's/^127\.0\.0\.1:\([0-9]*\)$/\1/p' | head -n 1)"
[ -n "$port" ] || { echo "FAIL: no published port" >&2; exit 1; }
# Tests that replay the migration chain itself (services/hub/src/pg_tests_chain.rs, one test) run only where that file is.
# Plus 8 adopt tests, which always use the baseline migrator (step 2.15).
n=23
[ ! -f services/hub/src/pg_tests_chain.rs ] || n=24
(cd services && HUB_TEST_DB_URI="postgres://postgres:x@127.0.0.1:$port/hub_rs" \
  cargo test --locked -q -p hub pg_tests -- --ignored) >"$out2" 2>&1 || { cat "$out2" >&2; exit 1; }
grep -q "test result: ok. $n passed" "$out2" || { echo "FAIL: pg_tests did not run $n tests" >&2; cat "$out2" >&2; exit 1; }
echo "ok: ticket fields, seed links, /api/graph, a scope plan and role boundaries on Postgres (queries as role hub)"

# 4b. HUB_MIGRATIONS=baseline (steps 2.12c, 2.15): the same tests on the baseline.
echo "CREATE DATABASE hub_rs_base;" | psql_ >/dev/null
(cd services && HUB_MIGRATIONS=baseline HUB_TEST_DB_URI="postgres://postgres:x@127.0.0.1:$port/hub_rs_base" \
  cargo test --locked -q -p hub pg_tests -- --ignored) >"$out2" 2>&1 || { cat "$out2" >&2; exit 1; }
grep -q "test result: ok. $n passed" "$out2" || { echo "FAIL: pg_tests with HUB_MIGRATIONS=baseline did not run $n tests" >&2; cat "$out2" >&2; exit 1; }
echo "ok: the same tests on the baseline, and hub migrate --adopt adopts the chain and refuses 6 kinds of difference"

# 5. Better Auth as role hub_web: sign-in, session, JWKS, MCP client registration; public.tickets is closed.
# Migration 0021 created the role without login; the password is only for this throwaway container.
echo "ALTER ROLE hub_web LOGIN PASSWORD 'x';" | psql_ >/dev/null
[ -d hub-web/node_modules ] || (cd hub-web && pnpm install --frozen-lockfile >/dev/null)
(cd hub-web && HUB_TEST_AUTH_DB_URI="postgres://hub_web:x@127.0.0.1:$port/hub" \
  node --test --test-reporter=tap src/lib/server/auth-pg.test.ts) >"$out2" 2>&1 || { cat "$out2" >&2; exit 1; }
grep -q '^# pass 1' "$out2" || { echo "FAIL: auth-pg test did not pass" >&2; cat "$out2" >&2; exit 1; }
echo "ok: Better Auth of hub-web works as role hub_web, public.tickets is closed to it"

# 6. Better Auth generator: the README step after a better-auth update. The auth tables from migration 0007 match the
# config, and an empty database gives CREATE TABLE: so the CLI loaded the config (with the #lib/* import) and read the database.
ver="$(sed -n 's/^ *"better-auth": "\([0-9.]*\)",*$/\1/p' hub-web/package.json)"
[ -n "$ver" ] || { echo "FAIL: no better-auth version in hub-web/package.json" >&2; exit 1; }
gen="$(mktemp -d)"
trap 'docker rm -f "$name" >/dev/null 2>&1 || true; rm -rf "$out1" "$out2" "$gen"' EXIT INT TERM
HUBDB_URI="postgres://postgres:x@127.0.0.1:$port/hub" \
  pnpm dlx "auth@$ver" generate --config hub-web/auth.cli.ts --output "$gen/hub.sql" -y >"$out2" 2>&1 || { cat "$out2" >&2; exit 1; }
grep -q 'already up to date' "$out2" || { echo "FAIL: auth generate wants changes on the migrated base" >&2; cat "$out2" >&2; exit 1; }
HUBDB_URI="postgres://postgres:x@127.0.0.1:$port/hub_rs_base" \
  pnpm dlx "auth@$ver" generate --config hub-web/auth.cli.ts --output "$gen/base.sql" -y >"$out2" 2>&1 || { cat "$out2" >&2; exit 1; }
grep -q 'already up to date' "$out2" || { echo "FAIL: auth generate wants changes on the baseline (0007 tables)" >&2; cat "$out2" >&2; exit 1; }
HUBDB_URI="postgres://postgres:x@127.0.0.1:$port/postgres" \
  pnpm dlx "auth@$ver" generate --config hub-web/auth.cli.ts --output "$gen/empty.sql" -y >"$out2" 2>&1 || { cat "$out2" >&2; exit 1; }
grep -qi 'create table "user"' "$gen/empty.sql" || { echo "FAIL: auth generate made no tables on an empty base" >&2; cat "$out2" >&2; exit 1; }
echo "ok: auth@$ver generate with hub-web/auth.cli.ts: up to date after the chain and the baseline, $(grep -ci 'create table' "$gen/empty.sql") tables on an empty base"
