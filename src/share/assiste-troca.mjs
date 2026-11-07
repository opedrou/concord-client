// Chrome assistindo enquanto a tela troca: uma amostra do <video> a cada 100 ms, e o resumo
// no fim. Quem chama é o `bancada_tela_troca`. Usa o puppeteer da bancada do spike.
import { createRequire } from 'node:module';
const require = createRequire(process.env.CONCORD_BANCADA + '/');
const puppeteer = require('puppeteer-core');

const { CONCORD_URL: base, CONCORD_ROOM: room, CONCORD_USER: user, CONCORD_PASS: pass, OUT } = process.env;
const espera = (ms) => new Promise((r) => setTimeout(r, ms));
const browser = await puppeteer.launch({
  executablePath: '/opt/google/chrome/chrome',
  headless: 'new',
  userDataDir: `${OUT}/perfil-troca`,
  args: ['--use-fake-ui-for-media-stream', '--use-fake-device-for-media-stream', '--autoplay-policy=no-user-gesture-required', '--no-first-run', '--mute-audio'],
});
const page = await browser.newPage();
await page.setViewport({ width: 1280, height: 760 });
await page.goto(`${base}/login`, { waitUntil: 'domcontentloaded' });
await page.evaluate(async (u, p) => { await fetch('/api/auth/login', { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ username: u, password: p }) }); }, user, pass);
await page.goto(`${base}/rooms/${room}`, { waitUntil: 'domcontentloaded' });
await espera(7000);
const clicou = await page.evaluate(() => {
  const b = [...document.querySelectorAll('button')].find((x) => x.textContent.trim() === 'Assistir');
  b?.click();
  return !!b;
});
console.log('CLICOU', clicou);
await espera(2000);
await page.evaluate(() => {
  window.amostras = [];
  const t0 = performance.now();
  setInterval(() => {
    const v = [...document.querySelectorAll('video')].find((x) => x.srcObject?.getVideoTracks?.().length);
    const tr = v?.srcObject.getVideoTracks()[0];
    window.amostras.push({ t: Math.round(performance.now() - t0), w: v?.videoWidth ?? 0, h: v?.videoHeight ?? 0, q: v?.getVideoPlaybackQuality().totalVideoFrames ?? 0, id: tr?.id ?? null, ok: tr?.readyState === 'live' });
  }, 100);
});
await espera(Number(process.env.JANELA_MS ?? 25000));
const a = await page.evaluate(() => window.amostras);
await page.screenshot({ path: `${OUT}/assiste-troca.png` });
await browser.close();

// Maior intervalo sem quadro novo, e cada tamanho com o instante em que apareceu.
let ultimo = a[0], pausa = 0;
const tamanhos = [];
for (const x of a) {
  if (x.q > ultimo.q) ultimo = x;
  pausa = Math.max(pausa, x.t - ultimo.t);
  const tam = `${x.w}x${x.h}`;
  if (tamanhos.at(-1)?.tam !== tam) tamanhos.push({ tam, t: x.t });
}
console.log('AMOSTRAS', a.length, 'quadros', a.at(-1).q - a[0].q, 'em', a.at(-1).t, 'ms');
console.log('FAIXAS', [...new Set(a.map((x) => x.id))].join(','));
console.log('SEM_VIDEO', a.filter((x) => !x.w || !x.ok).length);
console.log('TAMANHOS', tamanhos.map((x) => `${x.tam}@${x.t}`).join(' '));
console.log('MAIOR_PAUSA_MS', pausa);
