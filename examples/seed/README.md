# Example seed

A neutral seed directory for `HUB_SEED_DIR` (step 2.6 of the factory plan): the same set of files an install seed
has, with example data only. The hub tests read it (`services/hub/src/seed.rs` and the module tests), the local
AppHost run seeds from it, and a new install can start from it.

- `people.toml`: people, `graph.import_people`;
- `components.toml`: components, `graph.import_components`;
- `scopes.toml`: plan scopes, `plan.import_scopes`;
- `repositories.toml`: registry of document repositories, `public.import_document_repositories`;
- `links/*.jsonl`: ticket links in ascending file name order, `graph.import_links`.

The import only adds and updates: a row removed from a file stays in the database. Without the directory, or with
no seed file in it, hub starts without seeds; with `HUB_SEED_REQUIRED=true` it does not start.
