// Chrome na sala que muta o cliente "só pra mim" e desmuta, pro `bancada_avisos` ver o selo
// de "não está te ouvindo" chegar. Usa o puppeteer da bancada do spike.
import { createRequire } from 'node:module';
const require = createRequire(process.env.CONCORD_BANCADA + '/');
const puppeteer = require('puppeteer-core');

const { CONCORD_URL: base, CONCORD_ROOM: room, CONCORD_USER: user, CONCORD_PASS: pass, OUT } = process.env;
const espera = (ms) => new Promise((r) => setTimeout(r, ms));
const browser = await puppeteer.launch({
  executablePath: '/opt/google/chrome/chrome',
  headless: 'new',
  userDataDir: `${OUT}/perfil-avisos`,
  args: ['--use-fake-ui-for-media-stream', '--use-fake-device-for-media-stream', '--no-first-run', '--mute-audio'],
});
const page = await browser.newPage();
await page.setViewport({ width: 1280, height: 800 });
await page.goto(`${base}/login`, { waitUntil: 'domcontentloaded' });
await page.evaluate(async (u, p) => { await fetch('/api/auth/login', { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ username: u, password: p }) }); }, user, pass);
await page.goto(`${base}/rooms/${room}`, { waitUntil: 'domcontentloaded' });
await espera(10000);

// Botão direito no tile do cliente abre o card de volume; o botão de mudo é o primeiro.
const botao = async (prefixo) => {
  await page.click('[data-lk-local-participant="false"]', { button: 'right' });
  await espera(500);
  await page.click(`button[aria-label^="${prefixo}"]`);
  await page.keyboard.press('Escape');
  console.log('AVISOS', prefixo);
};
await botao('Mutar');
await espera(8000);
await page.screenshot({ path: `${OUT}/avisos.png` });
await botao('Desmutar');
await espera(8000);
// Sair navegando: o site desconecta no `pagehide`.
await page.goto('about:blank');
console.log('AVISOS SAIU');
await espera(2000);
await browser.close();
