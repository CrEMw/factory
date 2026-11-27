// The migrations the PGlite tests apply (step 2.12c of the factory plan): the chain services/hub/migrations by default,
// or with HUB_MIGRATIONS=baseline the baseline services/hub/baseline (what hub applies with the same env,
// docs/migrations.md); tools/test-baseline-pglite.mjs runs the tests that use this module that way. Tests that
// check data the chain wrote for an install (a backfill, registry rows, dictionary actors) read services/hub/migrations directly.
//   HUB_MIGRATIONS=baseline node --test tools/test-plan.mjs
import { readFile, readdir } from 'node:fs/promises';

const root = new URL('../', import.meta.url);

export const migrationsDir = process.env.HUB_MIGRATIONS === 'baseline' ? 'services/hub/baseline/' : 'services/hub/migrations/';

/** File names of the migrations in sqlx order (NNNN_name.sql; other files of the directory are not migrations). */
export const migrationFiles = async () =>
  (await readdir(new URL(migrationsDir, root))).filter((f) => /^\d+_.+\.sql$/.test(f)).sort();

/** The SQL text of one migration file. */
export const migrationSql = (file) => readFile(new URL(migrationsDir + file, root), 'utf8');
