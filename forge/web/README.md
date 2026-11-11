# cas forge web

The forge's web frontend: SvelteKit (TypeScript), built to static files and
served by `cas-forge serve --web forge/web/build`. Everything it shows comes
from the JSON API at `/api/v1`; it keeps nothing but the session token
cookie.

```sh
npm install
npm run routes         # makes src/routes from src/pages; build, check and dev run it too
npm run build          # writes build/
npm run check          # svelte-check
FORGE=https://localhost:8443 npm run dev   # proxies /api to a running forge
```

The pages are in `src/pages`, under names a cas directory holds:
`page.svelte` and `layout.*` stand for SvelteKit's `+page.svelte` and
`+layout.*`, and a directory `_owner` for `[owner]`. `routes.js` makes
`src/routes` (not kept in the repository) as links to them.
