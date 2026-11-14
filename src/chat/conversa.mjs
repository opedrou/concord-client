// Chrome no canal de texto do site, conversando com o `bancada_chat`: manda, espera ver o que
// o cliente mandou e tira um print. Usa o puppeteer da bancada do spike.
import { createRequire } from 'node:module';
const require = createRequire(process.env.CONCORD_BANCADA + '/');
const puppeteer = require('puppeteer-core');

const { CONCORD_URL: base, CONCORD_CANAL: canal, CONCORD_USER: user, CONCORD_PASS: pass, RUN: run, OUT } = process.env;
const espera = (ms) => new Promise((r) => setTimeout(r, ms));
const browser = await puppeteer.launch({
  executablePath: '/opt/google/chrome/chrome',
  headless: 'new',
  userDataDir: `${OUT}/perfil-chat`,
  args: ['--no-first-run', '--mute-audio'],
});
const page = await browser.newPage();
await page.setViewport({ width: 1100, height: 900 });
await page.goto(`${base}/login`, { waitUntil: 'domcontentloaded' });
await page.evaluate(async (u, p) => {
  await fetch('/api/auth/login', { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ username: u, password: p }) });
}, user, pass);
await page.goto(`${base}/channels/${canal}`, { waitUntil: 'domcontentloaded' });
const campo = 'textarea[aria-label^="Mensagem em"]';
await page.waitForSelector(campo, { timeout: 60000 });
// O EventSource abre depois de a página montar.
await espera(2000);

const mandar = async (texto) => {
  await page.type(campo, texto);
  await page.keyboard.press('Enter');
  console.log('WEB MANDOU', texto);
};
const ver = async (texto, ms = 60000) => {
  await page.waitForFunction((t) => document.body.innerText.includes(t), { timeout: ms }, texto);
  console.log('WEB VIU', texto);
};

await mandar(`web-1 ${run}`);
await ver(`cliente-1 ${run}`);
// O SSE do Chrome não cai: as mensagens da queda do cliente chegam aqui na hora.
await ver(`durante-5 ${run}`, 90000);
await page.screenshot({ path: `${OUT}/chat-web.png` });
await browser.close();
console.log('WEB OK');
