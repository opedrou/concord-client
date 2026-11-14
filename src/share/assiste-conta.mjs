// Entra na sala pelo site e imprime o rótulo de espectadores de cada tile de transmissão, o
// que o site leu do `concord.watching`. Quem chama é o `bancada_assistindo`. Usa o puppeteer
// da bancada do spike.
import { createRequire } from 'node:module';
const require = createRequire(process.env.CONCORD_BANCADA + '/');
const puppeteer = require('puppeteer-core');

const { CONCORD_URL: base, CONCORD_ROOM: room, CONCORD_USER: user, CONCORD_PASS: pass, OUT } = process.env;
const espera = (ms) => new Promise((r) => setTimeout(r, ms));
const browser = await puppeteer.launch({
  executablePath: '/opt/google/chrome/chrome',
  headless: 'new',
  userDataDir: `${OUT}/perfil-conta`,
  args: ['--use-fake-ui-for-media-stream', '--use-fake-device-for-media-stream', '--no-first-run', '--mute-audio'],
});
const page = await browser.newPage();
await page.setViewport({ width: 1280, height: 760 });
await page.goto(`${base}/login`, { waitUntil: 'domcontentloaded' });
await page.evaluate(async (u, p) => { await fetch('/api/auth/login', { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ username: u, password: p }) }); }, user, pass);
await page.goto(`${base}/rooms/${room}`, { waitUntil: 'domcontentloaded' });
await espera(Number(process.env.ESPERA_MS ?? 9000));
const rotulos = await page.evaluate(() =>
  [...document.querySelectorAll('[role="img"][aria-label]')]
    .map((e) => e.getAttribute('aria-label'))
    .filter((l) => l.includes('assistindo')));
console.log('ROTULOS', JSON.stringify(rotulos));
await page.screenshot({ path: `${OUT}/assiste-conta.png` });
await browser.close();
