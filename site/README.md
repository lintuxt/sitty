# site

Teaser page served at https://sitty.si.

Static, no build step: `index.html` + `favicon.svg`. Deployed by Cloudflare Pages on push to `trunk` (root directory: `site`, build command: none).

Preview locally:

```sh
python3 -m http.server 8000 --directory site
```
