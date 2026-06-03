# site-index

Static landing page for awsm-renderer that links to the sibling
frontends (model-tests + scene-editor). `index.html` is generated
at build time from `index.template.html` with `envsubst` — URLs come
from the env vars wired through `taskfiles/config.yml`.

## Adding a new site

1. Add `URL_PROD_<NAME>` + `URL_DEV_<NAME>` to `taskfiles/config.yml`.
2. Add a `<li>` to `index.template.html`.
3. Add the env var to the `env:` block in both `build` and
   `dev-build` in `taskfiles/frontend/site-index.yml`.

No rebuild of the wasm sites is needed when URLs change — just re-run
`task site-index:build`.

## Tooling

`envsubst` ships with GNU gettext. On macOS:

```
brew install gettext
brew link --force gettext   # exposes /opt/homebrew/bin/envsubst
```

(`brew link --force` is safe — gettext is keg-only because the macOS
SDK ships an unrelated `envsubst` placeholder, not because it
conflicts with anything you care about.)
