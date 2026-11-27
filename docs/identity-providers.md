# Identity providers

How people sign in to hub-web. Code: `hub-web/src/lib/server/identity/` (providers), `hub-web/src/lib/server/auth.ts`
and `auth-config.ts` (Better Auth setup), `hub-web/src/lib/server/oauth-flow.ts` (sign-in steps).

## Model

hub-web uses [Better Auth](https://github.com/better-auth/better-auth) with its tables in the `auth` schema of the hub
database. There are no passwords and no sign-up form (`emailAndPassword` is off). A person is identified by an email
address that comes from an identity provider; the first sign-in creates the user, later ones reuse the user with the
same email. Sessions last 30 days.

The hub is also an OAuth authorization server for MCP clients (the Better Auth `mcp` plugin): a client registers
itself, sends the browser to `/login`, the person signs in through a provider and confirms on `/consent`, and the
client gets a token for `<ORIGIN>/mcp`. `/login/fresh` re-signs a browser in when a page needs a sign-in no older
than 5 minutes.

## Providers

Settings come from env, names `AUTH_*`. A provider whose settings are missing is off. With none enabled there is no
way to sign in.

| Id | Source of identity | Enabled by |
|---|---|---|
| `oidc` | An external OpenID Connect provider, browser redirect | `AUTH_OIDC_ISSUER`, `AUTH_OIDC_CLIENT_ID` and a client secret |
| `cloudflare-access` | A Cloudflare Access JWT in `Cf-Access-Jwt-Assertion` | `AUTH_CF_ACCESS_TEAM_DOMAIN` and `AUTH_CF_ACCESS_AUD` |
| `tailscale` | Tailscale proxy headers `Tailscale-User-Login`, `Tailscale-User-Name` | `AUTH_TAILSCALE_HOSTS` |
| `dev` | A fixed login, for local runs | `AUTH_DEV_LOGIN` |

`cloudflare-access`, `tailscale` and `dev` are header providers: they read the identity from the request sent by the
entrance in front of the hub. Each one is trusted only on its own hosts (the `Host` header): on another host anyone
could send the same headers. They are checked in the order tailscale, cloudflare-access, dev; the first provider whose
host matches decides alone, so a failed check is not a fall-through to the next header provider. When the header
providers give no identity and `oidc` is enabled, the browser goes to the OIDC provider; otherwise sign-in fails.

Trust boundary: a client that reaches hub-web directly (for example another pod in the same namespace, if network
policy allows it) can set the right `Host` itself and send Tailscale headers.

### OIDC

Sign-in at an external OpenID Connect provider through the Better Auth `genericOAuth` plugin, with PKCE. Issuer and
keys come from `<issuer>/.well-known/openid-configuration`, and the identity only from a verified ID token.

| Variable | Default | Meaning |
|---|---|---|
| `AUTH_OIDC_ISSUER` | none | Issuer URL, must be `https`. A trailing `/` is removed. |
| `AUTH_OIDC_CLIENT_ID` | none | Client id registered at the provider. |
| `AUTH_OIDC_CLIENT_SECRET` | none | Client secret. |
| `AUTH_OIDC_CLIENT_SECRET_FILE` | none | File with the client secret, used when `AUTH_OIDC_CLIENT_SECRET` is unset. |
| `AUTH_OIDC_SCOPES` | `openid profile email` | Scopes, space or comma separated. |
| `AUTH_OIDC_NAME` | `OIDC` | Provider name shown on the sign-in pages. |

Register the redirect address `<ORIGIN>/api/auth/callback/oidc` at the provider.

Every user the provider admits can sign in, and Better Auth links an OIDC account to an existing hub user with the
same email when the provider reports the email as verified. Use only a provider whose user directory you control.

The code has no provider specific to GitHub or any other service. A service that offers a standard OpenID Connect
issuer (or an OIDC broker in front of it) works through `oidc`.

### Cloudflare Access

Cloudflare Access admits people by its own policy and signs each request with a JWT. hub-web checks the RS256
signature against the keys at `https://<team domain>/cdn-cgi/access/certs`, the issuer `https://<team domain>` and the
audience, and takes the `email` claim.

| Variable | Default | Meaning |
|---|---|---|
| `AUTH_CF_ACCESS_TEAM_DOMAIN` | none | Access team domain (host name only, no scheme). |
| `AUTH_CF_ACCESS_AUD` | none | Application audience tag. |
| `AUTH_CF_ACCESS_HOSTS` | host of `ORIGIN` | Hosts on which the JWT is accepted, comma separated. |

### Tailscale

The Tailscale proxy removes incoming `Tailscale-User-*` headers and sets its own for user devices. Tagged devices get
none and cannot sign in. A non-ASCII name in RFC 2047 encoding is decoded.

| Variable | Default | Meaning |
|---|---|---|
| `AUTH_TAILSCALE_HOSTS` | none | Hosts on which the headers are trusted, comma separated. |

### Development login

| Variable | Default | Meaning |
|---|---|---|
| `AUTH_DEV_LOGIN` | none | Email every request signs in as, on any host that has no other header provider. Refused with `NODE_ENV=production`: hub-web does not start. |

The AppHost sets `AUTH_DEV_LOGIN` to a fixed local login for `aspire run`.

## Other settings

| Variable | Default | Meaning |
|---|---|---|
| `AUTH_EXTRA_ORIGINS` | none | Other addresses of the same hub besides `ORIGIN`, comma separated. Forms and Better Auth calls with cookies are accepted from them. The `https://` addresses of the Tailscale and Cloudflare Access hosts are trusted as well. |
| `TRUSTED_PROXY_CIDRS` | `10.42.0.0/16,10.43.0.0/16` | Proxy networks used to find the client address in `X-Forwarded-For` for the Better Auth rate limit. |
| `MCP_OPEN_CLIENT_REGISTRATION` | `true` | Allow MCP client registration without sign-in. |

## In the chart

The chart (`deploy/chart`) wires these variables into hub-web:

| Variable | Source |
|---|---|
| `AUTH_TAILSCALE_HOSTS` | values `auth.providers.tailscale.host`, through the ConfigMap of hub-web |
| `AUTH_EXTRA_ORIGINS` | values `auth.extraOrigins`, through the ConfigMap of hub-web |
| `AUTH_CF_ACCESS_TEAM_DOMAIN`, `AUTH_CF_ACCESS_AUD` | secret `hub-web-access` (keys `team_domain`, `aud`), only when `auth.providers.cloudflareAccess.enabled`; the ExternalSecret reads `secrets.remoteKeys.accessTeamDomain` and `secrets.remoteKeys.accessAud` |
| `BETTER_AUTH_SECRET` | secret `hub-web-auth`, key `password` (`secrets.remoteKeys.webAuthSecret`) |
| `AUTH_OIDC_CLIENT_SECRET_FILE` | file of the secret `hub-web-oidc` (`secrets.remoteKeys.oidcClientSecret`), only when that key is set |
| `ORIGIN` | values `origin` |

The chart has no dedicated keys for `AUTH_OIDC_*`, `AUTH_CF_ACCESS_HOSTS` or `TRUSTED_PROXY_CIDRS`. Non-secret
settings go through `web.env`:

```yaml
origin: https://hub.example.com
web:
  env:
    AUTH_OIDC_ISSUER: https://id.example.com
    AUTH_OIDC_CLIENT_ID: hub
    AUTH_OIDC_NAME: Example ID
```

`web.env` values are plain env entries in the Deployment, not secrets, and the chart refuses to render a `web.env` or
`hub.env` name that contains `SECRET`, `PASSWORD`, `TOKEN` or `PRIVATE_KEY`. The OIDC client secret comes from the
secret store: set `secrets.remoteKeys.oidcClientSecret` to the name of its key there. The chart then renders the
ExternalSecret `hub-web-oidc`, mounts it read-only into hub-web and sets `AUTH_OIDC_CLIENT_SECRET_FILE`. hub-web reads
the file at start, so a new secret value takes effect on the next restart.

```yaml
secrets:
  remoteKeys:
    oidcClientSecret: hub-web-oidc-client-secret
```
