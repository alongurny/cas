# cas forge web

The forge's web frontend: SvelteKit (TypeScript), built to static files and
served by `cas-forge serve --web forge/web/build`. Everything it shows comes
from the JSON API at `/api/v1`; it keeps nothing but the session token
cookie.

```sh
npm install
npm run build          # writes build/
npm run check          # svelte-check
FORGE=https://localhost:8443 npm run dev   # proxies /api to a running forge
```
