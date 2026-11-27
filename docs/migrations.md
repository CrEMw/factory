# Hub database migrations: chain, baseline and adopt

Steps 2.12 and 2.15 of the factory plan. Code: `services/hub/src/migrations.rs`, `services/hub/src/adopt.rs`.

## Two migrators

- **Chain** (`HUB_MIGRATIONS` unset, empty or `chain`): `services/hub/migrations/NNNN_*.sql`, versions 1-14 and 17-29 (15 and 16 were never
  merged). Files are frozen: sqlx stores a SHA-384 checksum of each file in `public._sqlx_migrations`, and the
  service refuses to start when a checksum differs (`migrations::verify`).
- **Baseline** (`HUB_MIGRATIONS=baseline`): `services/hub/baseline/1000_baseline.sql`, one file, version 1000. It
  gives the same catalog as the whole chain (`sh tools/test-pg-schema.sh`). It carries the schema and its
  dictionaries, not the rows of installation registries: those come from the seed directory (`HUB_SEED_DIR`).

One build carries both; env `HUB_MIGRATIONS` of `hub migrate` and of the service picks one, any other value is
refused before connecting:

```sh
hub migrate                                   # chain
HUB_MIGRATIONS=baseline hub migrate --adopt   # adopt once, then the baseline
```

In the Helm chart the values `hub.migrations.source` and `hub.migrations.adoptChecksums` set both (docs/configuration.md).

Both migrators set `ignore_missing`, so the baseline ignores the chain versions left in an adopted journal, and the chain ignores version 1000.

A new database with `HUB_MIGRATIONS=baseline` gets version 1000 from plain `hub migrate`. A database built by the
chain needs `hub migrate --adopt` once; plain `hub migrate` with `HUB_MIGRATIONS=baseline` refuses such a database instead of
running the baseline over it. An adopted database may go back to `HUB_MIGRATIONS=chain` (its journal still has the chain versions). A database
built by the baseline alone must not: the chain would try to run 0001 over the existing schema (it fails and rolls
back).

## `hub migrate --adopt`

Only with `HUB_MIGRATIONS=baseline`; otherwise `hub migrate --adopt` answers with an error. It runs where `hub migrate` runs (the Job
`hub-db-migrate`, as the schema owner), then continues as `hub migrate` (baseline migrator, instance seed).

In one transaction, under the advisory lock that sqlx `Migrator::run` takes (`Migrate::lock` on the same
connection; sqlx 0.8 derives the key from `current_database()` with CRC-32, so a parallel `hub migrate` waits):

1. Version 1000 is in the journal: nothing to do, exit 0.
2. The journal versions below 1000 must be exactly the versions of `HUB_ADOPT_CHECKSUMS`, each with
   `success = true` and the listed checksum. An extra version, a missing one, `success = false`, a different
   checksum or any version above 1000 is a refusal.
3. The schema catalog of the database (`services/hub/pg-schema.sql`) must equal `services/hub/baseline/catalog.txt` byte
   for byte: function bodies and attributes, views, comments, tables, columns, defaults, constraints, indexes,
   triggers, grants, default privileges, sequences, types, extensions.
4. Then `INSERT INTO public._sqlx_migrations VALUES (1000, 'baseline', true, <sha384 of 1000_baseline.sql>, 0)` and
   commit.

On a refusal nothing is written: the report of differences goes to stderr (`journal: ...` lines and the catalog
diff, `-` expected, `+` this database, at most 200 lines) and `hub` exits with 1.

### HUB_ADOPT_CHECKSUMS

One line per chain migration, `<version> <sha384 hex>`; blank lines and lines starting with `#` are skipped. The
list comes from the reviewed migration files, not from the database being adopted:

```sh
for f in services/hub/migrations/[0-9]*.sql; do
  v="$(basename "$f" | sed 's/_.*//; s/^0*//')"
  echo "$v $(sha384sum "$f" | cut -d' ' -f1)"
done
```

### The expected catalog

`services/hub/baseline/catalog.txt` is the output of `services/hub/pg-schema.sql` on an empty database with only the
baseline applied; `hub` embeds it at build time. Regenerate it with the baseline:

```sh
sh tools/test-pg-schema.sh --write-catalog
```

`sh tools/test-pg-schema.sh` (the `pg-schema` part of `tools/check.sh`, local only: it needs docker) fails when the
file is stale. Why an embedded file and not a comparison at adopt time: building the expected catalog needs the
baseline applied to another database (CREATEDB, which the migration role does not have) or next to the live schema
(the same schema names). Deparsed texts are stable within a Postgres major version; the file comes from Postgres 18,
and on another major the report says so.

## Tests

- `sh tools/test-graph-pg.sh` (part `graph-pg`, local, docker): the Rust Postgres tests with the chain plus the adopt
  tests (always on the baseline migrator), then the same with `HUB_MIGRATIONS=baseline`. The adopt tests: adopt of a chain database and a repeat; refusal of a
  foreign checksum, version 999, a missing version, `success = false`, an extra column, a function comment; a new
  database gets the baseline the normal way; and `auth generate` answers "already up to date" on the baseline too.
- `cargo test -p hub adopt`: parsing of `HUB_ADOPT_CHECKSUMS` and the journal comparison.
- `tools/test-baseline-pglite.mjs`: the PGlite tests that use `tools/hub-migrations.mjs` with
  `HUB_MIGRATIONS=baseline`.
