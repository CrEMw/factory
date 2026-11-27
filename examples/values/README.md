# Example values

Values files for the Helm chart in `deploy/chart`. Each one sets only keys of `deploy/overlay/values.yaml`, which
holds the defaults and explains every key. Render one with:

```sh
helm template factory deploy/chart -f examples/values/minimal.yaml
```

| File | Entrance | Sign-in |
| --- | --- | --- |
| `minimal.yaml` | none: bring your own Ingress or Gateway to `hub-web-service` | external OpenID Connect provider |
| `cloudflare-access.yaml` | cloudflared connector in the cluster (Cloudflare Tunnel) | Cloudflare Access JWT |
| `tailscale.yaml` | Tailscale operator Ingress | Tailscale proxy headers |

All of them set the two required install settings, `hub.instance.account` and `hub.instance.timezone`, and expect
CloudNativePG, External Secrets Operator and a `SecretStore` named `hub-secrets` that holds the keys in
`secrets.remoteKeys`. Container images are not set here: they are the `parameters` of the generated chart
(`deploy/chart/values.yaml`).

The entrances combine: the overlay values of `cloudflare-access.yaml` and `tailscale.yaml` can go into one file, and
`web.env` from `minimal.yaml` adds OIDC to either. Add other addresses of the same hub to `auth.extraOrigins`.
