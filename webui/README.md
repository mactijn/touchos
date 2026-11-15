# touchos settings page

The browser app behind `http://<panel IP>/`: Angular with Angular Material 3, standalone components, signals, no zone.js.
The panel's web server (`servers/web`) embeds the build output from `../build/webui`.

```sh
cargo xtask webui                              # build (cargo xtask build does this too when something here changed)
cargo xtask webui serve http://<panel-ip>      # live reload on http://localhost:4200, /api forwarded to that panel
npx ng test --watch=false                  # unit tests (vitest)
```

`npm start` is the same as `serve`. It reads the panel's address from `TOUCHOS_PANEL` (default `http://localhost:8080`, QEMU).

## Rules that come from the panel

- **Styles need the CSP nonce.** The panel allows inline `<style>` only with the nonce it writes into `index.html` (`ngCspNonce="__TOUCHOS_CSP_NONCE__"`, replaced per request). Angular adds it to every style it inserts. Don't set `style="…"` attributes in templates; `[style.x]` bindings are fine.
- **Nothing from the internet.** Icons are Material Symbols SVGs bundled by `src/app/icons.ts` (add a name there to use one); the font is the system font.
- **No lazy loading.** The panel answers one request per connection; one script loads faster than several.
- **The `X-Touchos` header** is on every request (`touchosHeader` in `src/app/api.ts`). The panel refuses changes without it.
- **Upload progress needs XHR,** so the app uses `provideHttpClient(withXhr())`.

## Layout

| File | What |
|---|---|
| `src/app/api.ts` | API types and `Api`, which returns `{status, data, err}` instead of throwing |
| `src/app/session.ts` | Pairing state, route guards, the unsaved-changes guard |
| `src/app/config.ts` | Schema, settings and saving (version conflicts, field errors) |
| `src/app/form-values.ts` | Form texts ↔ typed values, and the change list for a save (unit-tested) |
| `src/app/settings-form.ts` | The form for all sections and the tiles, kept across pages; saving it |
| `src/app/setting-field.ts` | One setting, with the control that fits its kind |
| `src/app/icons.ts` | The bundled icons |
| `src/app/panel.ts` | Status for the header line; waiting for the panel after a restart |
| `src/app/update.ts` | Uploading an update bundle with progress |
| `src/app/app.ts` | The frame: side bar, top bar with status, save bar |
| `src/app/pages/` | `pair`, `section` (one settings section), `tiles`, `software`, `system` |
