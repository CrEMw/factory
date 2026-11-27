# Rules for coding agents

These rules apply to every change in this repository, by people and by agents. Layout and quick start:
[README.md](README.md).

## Before a pull request

- Run `tools/check.sh` from the repository root. Without arguments it runs the parts the branch touches (diff against
  `origin/main`, plus uncommitted and new files); `tools/check.sh --all` runs everything; `tools/check.sh <part>...`
  runs the named parts with full output.
- Parts: `rust`, `rust-lint`, `web`, `node-tests`, `chart`, `pg-schema`, `cyrillic` (the gate parts), and `apphost`,
  `helm-lint`, `graph-pg` (local only; `graph-pg` and the catalog comparison in `pg-schema` need Docker). The list
  and what each part does are in the header of `tools/check.sh`.
- A new check goes into `tools/check.sh` as a part, not straight into a CI workflow.
- Do not open a PR with a failing part. If a part cannot run on your machine, say so in the PR.

## Database migrations

- Never edit a migration that is already applied anywhere. The service compares the checksum of every applied
  migration and refuses to start when one differs.
- A schema change is a new numbered migration file. How the migrators, the baseline and adopt work:
  [docs/migrations.md](docs/migrations.md).
- Migrations only move forward. At startup `hub` accepts a database with versions newer than its image, so a schema
  change must keep a running older `hub` working.

## The Helm chart is generated

- Never edit `deploy/chart` by hand. Change `apphost/apphost.mts` or `deploy/overlay`, then run `sh tools/chart.sh`
  and commit both the source and the regenerated `deploy/chart`. The `chart` part fails when they drift.
- Neutral defaults belong in `deploy/overlay/values.yaml`. Nothing specific to one installation goes there: an
  installation passes its own values file.
- The markers that `tools/chart.sh` turns into Helm text (`'{{= X }}'`, `x-helm-if`, `*.tpl` files) are described
  in its header.

## UI strings

- Every user-visible string in `hub-web` goes through i18n (`hub-web/src/lib/i18n`): add the key to the English
  dictionary, the default (`hub-web/src/lib/i18n/en.ts` and its group modules `en/<group>.ts`), and its Russian
  translation to `hub-web/src/lib/i18n/ru.ts`.
- `hub-web/src/lib/i18n/ru.ts` is the only place where Cyrillic text is allowed; the `cyrillic` part enforces this.

## Language

English for code, comments, identifiers, commit messages, pull requests and documentation.

## Secrets

- Never commit secrets, tokens, passwords, private keys or real connection strings, not even in tests or examples.
- In the cluster secrets come from a secret store through External Secrets (`secrets.remoteKeys` in the values);
  locally Aspire generates them.
- Use neutral examples: account `acme`, people `alice` and `bob`, addresses under `example.com`.
