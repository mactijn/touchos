// `npm start` serves the app on http://localhost:4200 and forwards /api to a
// panel: TOUCHOS_PANEL=http://<panel-ip> (a Toon) or, by default,
// http://localhost:8080 (QEMU, `cargo xtask qemu`).
//
// The panel only accepts its own Host and Origin (DNS rebinding and CSRF
// defenses), so the proxy presents itself as the panel's own page.
const target = process.env.TOUCHOS_PANEL ?? 'http://localhost:8080';

export default {
  '/api': {
    target,
    changeOrigin: true,
    headers: { Origin: target },
  },
};
