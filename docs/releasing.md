# Releasing

A release is a semver tag `vX.Y.Z` on `main`. Before 1.0 an incompatible change raises the minor version.

## Checks

Every pull request and every push to `main` run `.github/workflows/ci.yml`:

- `dagger`: `dagger call check --source=.`, the CI parts of `tools/check.sh` in the tools image. Run the same
  command locally before a pull request.
- `leak`: gitleaks over the whole history with `.gitleaks.toml`, GitHub noreply addresses for authors and
  committers, no Cyrillic in the tree, a printable ASCII title and branch of the pull request.

## Cutting a release

A maintainer creates the tag through the GitHub API, never with a local `git tag`:

```sh
gh release create vX.Y.Z --target main --title vX.Y.Z --notes ""
```

`.github/workflows/release.yml` then:

1. builds `factory-hub`, `factory-hub-web` and `factory-tools` on native amd64 and arm64 runners with
   `dagger call publish` and joins each into one multi-arch index `X.Y.Z` (`ghcr.io/cremw/factory-hub`,
   `ghcr.io/cremw/factory-hub-web`, `ghcr.io/cremw/factory-tools`);
2. attaches an SPDX SBOM (syft) and build provenance as attestations and signs each index with keyless cosign;
3. pushes the chart to `oci://ghcr.io/cremw/charts/factory` with chart version and appVersion `X.Y.Z`, signed too;
4. writes the digests of the images and the chart into the GitHub Release.

A wrong release is fixed by the next patch release; a published tag is never moved.

## Verifying

```sh
cosign verify ghcr.io/cremw/factory-hub@sha256:<digest> \
  --certificate-identity-regexp '^https://github.com/CrEMw/factory/.github/workflows/release.yml@refs/tags/v' \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com
gh attestation verify oci://ghcr.io/cremw/factory-hub@sha256:<digest> -R CrEMw/factory
```

## Installing a release

An installation pins the chart version and the image digests in its own values and changes them only by a pull
request to its own repository. Schema migrations only go forward from version 1000 (docs/migrations.md).
