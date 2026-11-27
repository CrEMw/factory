/**
 * Checks of the factory tree: the parts of tools/check.sh in the tools image (tools/Dockerfile).
 *
 *   dagger call check --source=.
 *
 * The same command runs locally and in the `dagger` job of .github/workflows/ci.yml. `image` and `publish` build the
 * release images of .github/workflows/release.yml.
 */
import { argument, dag, Container, Directory, func, object, Platform, Secret } from "@dagger.io/dagger"

// The parts the CI gate runs (tools/check.sh). graph-pg is not here: it starts its own Postgres with docker, run it
// locally with `sh tools/check.sh graph-pg`.
const PARTS = [
  "rust",
  "rust-lint",
  "web",
  "node-tests",
  "chart",
  "apphost",
  "helm-lint",
  "pg-schema",
  "cyrillic",
  "factory-tree",
]

// What the build leaves in a working copy: never part of the checked source.
const IGNORE = [
  "**/node_modules",
  "**/target",
  "**/.svelte-kit",
  "hub-web/build",
  "apphost/.aspire",
  "apphost/.modules",
  "dagger/sdk",
]

const OWNER = "1000:1000"

// Release images: name in the registry, Dockerfile and build context.
const IMAGES: Record<string, { dockerfile: string; context: string }> = {
  hub: { dockerfile: "Dockerfile", context: "services" },
  "hub-web": { dockerfile: "Dockerfile", context: "hub-web" },
  tools: { dockerfile: "tools/Dockerfile", context: "." },
}

@object()
export class Factory {
  /**
   * The tools image with the source in /src, owned by the image user ci (uid 1000). Cargo and pnpm caches are
   * Dagger cache volumes, so a second run does not download or compile everything again.
   */
  @func()
  tools(@argument({ defaultPath: "/", ignore: IGNORE }) source: Directory): Container {
    return source
      .dockerBuild({ dockerfile: "tools/Dockerfile" })
      .withMountedCache("/cache/cargo-home", dag.cacheVolume("factory-cargo-home"), { owner: OWNER })
      .withMountedCache("/cache/cargo-target", dag.cacheVolume("factory-cargo-target"), { owner: OWNER })
      .withMountedCache("/cache/pnpm", dag.cacheVolume("factory-pnpm"), { owner: OWNER })
      .withEnvVariable("CARGO_HOME", "/cache/cargo-home")
      .withEnvVariable("CARGO_TARGET_DIR", "/cache/cargo-target")
      .withEnvVariable("npm_config_store_dir", "/cache/pnpm")
      .withDirectory("/src", source, { owner: OWNER })
      .withWorkdir("/src")
  }

  /**
   * The CI parts of tools/check.sh, or only the named ones (--parts=web,chart); fails on the first failing part, with
   * its output. factory-tree needs the source to be a git clone (`git archive HEAD`), not a git worktree.
   */
  @func()
  async check(
    @argument({ defaultPath: "/", ignore: IGNORE }) source: Directory,
    parts: string[] = PARTS,
  ): Promise<string> {
    return this.tools(source)
      .withExec(["sh", "tools/check.sh", ...parts])
      .stdout()
  }

  /**
   * One release image for one platform: component is hub, hub-web or tools.
   */
  @func()
  image(
    @argument({ defaultPath: "/", ignore: IGNORE }) source: Directory,
    component: string,
    platform: string = "linux/amd64",
  ): Container {
    const spec = IMAGES[component]
    if (!spec) throw new Error(`unknown component ${component}, expected one of ${Object.keys(IMAGES).join(", ")}`)
    const context = spec.context === "." ? source : source.directory(spec.context)
    return context.dockerBuild({ dockerfile: spec.dockerfile, platform: platform as Platform })
  }

  /**
   * Builds the three release images for one platform and pushes them as <registry>/factory-<component>:<tag>.
   * Returns one "<component> <reference with digest>" line each. The release job runs it once per architecture on a
   * native runner (the tools image does not build under emulation) and joins the tags into one index.
   */
  @func()
  async publish(
    @argument({ defaultPath: "/", ignore: IGNORE }) source: Directory,
    registry: string,
    tag: string,
    username: string,
    password: Secret,
    platform: string = "linux/amd64",
  ): Promise<string> {
    const host = registry.split("/")[0]
    const lines: string[] = []
    for (const component of Object.keys(IMAGES)) {
      const ref = await this.image(source, component, platform)
        .withRegistryAuth(host, username, password)
        .publish(`${registry}/factory-${component}:${tag}`)
      lines.push(`${component} ${ref}`)
    }
    return lines.join("\n")
  }
}
