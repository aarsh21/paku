# Optional self-hosting

Paku has no hosted service or public install domain. Local desktop/headless operation does not require the edge worker. These inherited deployment seams are for operators prepared to maintain their own authentication and storage; they are not a managed service or a guaranteed stable hosting API.

## Local validation

```sh
npm --prefix edge ci
npm --prefix edge run typecheck
npm --prefix edge test
npm --prefix edge run dev
```

`npm run dev` explicitly selects development bearer authentication on localhost. A bearer of `user@org` identifies a test user and organization; it is **not secure authentication**. Never publish an edge with `AUTH_MODE=dev`. Test configs disable `workers_dev` and have no routes. Keep local Wrangler ports private.

## An operator-owned deployment

1. Authenticate Wrangler to your own Cloudflare account. Supply `CLOUDFLARE_ACCOUNT_ID` yourself (or select your account through Wrangler). No upstream account ID is checked in.
2. Review `edge/wrangler.jsonc`. Its new names (`paku-edge`, `paku-blobs`, `paku-releases`) create independent resources; choose unique names in your account and create both R2 buckets. Durable Object migrations must be reviewed before deploying. These configs do not migrate upstream storage.
3. Create your own WorkOS tenant/application. Set `WORKOS_CLIENT_ID` in worker vars and `WORKOS_API_KEY` as a Wrangler secret. Keep `AUTH_MODE=workos`. An empty client ID fails closed. Configure your own callback URL and organization/auth policies; never reuse upstream credentials. The auth API is WorkOS-specific; endpoint overrides alone do not turn it into generic OIDC.
4. Configure client endpoint and WorkOS settings for that deployment (the `PAKU_EDGE_URL` and `PAKU_WORKOS_CLIENT_ID` environment/configuration seams). Do not point clients at an upstream service. Restart the engine when changing workspace scope. Endpoint configuration is not a stable supported migration contract.
5. Only add custom domain routes you own. The templates contain none. The static landing worker is optional. For the optional redirect worker, set `CANONICAL_ORIGIN` to your own HTTPS origin and configure your own route; it returns 503 until configured.
6. Optional APNs requires **all four** of `APNS_KEY_P8`, `APNS_KEY_ID`, `APNS_TEAM_ID`, and `APNS_TOPIC`, using your own Apple application. No upstream team or topic defaults are used. Without complete configuration, push delivery is disabled.

The deployment workflow runs checks on pushes but deploys only on a manual dispatch with `deploy=true`, repository variable `PAKU_DEPLOY_ENABLED=true`, and your `CLOUDFLARE_API_TOKEN`/`CLOUDFLARE_ACCOUNT_ID` secrets. Do not enable it before configuring authentication and resources. Cloudflare tokens need only permissions appropriate for resources in your account.

## Releases

The release workflow packages artifacts and publishes tagged releases to the current GitHub repository. It does not upload to an upstream R2 bucket or claim that downloads already exist. Maintainers can separately populate their own `paku-releases` bucket with versioned artifacts, `manifest.json` and `latest.txt` if they intentionally support the optional edge release endpoint.

`edge/src/install.sh` requires an explicit `PAKU_BASE_URL` release origin (or `file://` for offline fixtures). It has no default public host. Never offer the install endpoint until the corresponding artifacts and checksums are available. The presence of packaging/update machinery is not a guarantee of a public update feed.

## Trust and storage

Devices using the same account are trusted peers and can read/write each other's workspaces, including ignored files when requested. Local profiles do not automatically publish existing sessions after login. Back up your R2 and Durable Object data; deleting or renaming a worker can orphan its room storage. Test restoring snapshots and client/edge compatibility before rollout.
