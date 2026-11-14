// Chrome no canal de texto do site, conversando com o `bancada_chat`: manda, espera ver o que
// o cliente mandou e tira um print. Usa o puppeteer da bancada do spike.
import { createRequire } from 'node:module';
const require = createRequire(process.env.CONCORD_BANCADA + '/');
const puppeteer = require('puppeteer-core');

const { CONCORD_URL: base, CONCORD_CANAL: canal, CONCORD_USER: user, CONCORD_PASS: pass, RUN: run, OUT, FOTO } = process.env;
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

// Foto pelo botão de anexar: sobe na hora, e o Enter manda com a legenda.
const arquivo = await page.$('input[type=file]');
await arquivo.uploadFile(FOTO);
await ver('foto-web.png');
await mandar(`web-anexo ${run}`);

// Imagem do cliente aparece carregada, e o arquivo dele como cartão.
await page.waitForFunction(
  (alt) => [...document.images].some((i) => i.alt === alt && i.complete && i.naturalWidth === 64),
  { timeout: 60000 },
  `cliente-${run}.png`,
);
console.log('WEB VIU a imagem do cliente');
await ver(`notas-${run}.txt`);

// O cliente apaga o arquivo: some daqui pelo SSE.
await page.waitForFunction((t) => !document.body.innerText.includes(t), { timeout: 60000 }, `notas-${run}.txt`);
console.log('WEB VIU o arquivo sumir');

// Apaga a própria primeira pelo botão, com a confirmação.
const linha = await page.evaluateHandle((t) => [...document.querySelectorAll('li')].find((li) => li.innerText.includes(t)), `web-1 ${run}`);
await linha.hover();
await (await linha.$('button[aria-label="Apagar mensagem"]')).click();
const confirmar = await page.waitForFunction(() => [...document.querySelectorAll('button')].find((b) => b.innerText === 'Confirmar'));
await confirmar.click();
await page.waitForFunction((t) => !document.body.innerText.includes(t), { timeout: 30000 }, `web-1 ${run}`);
console.log('WEB APAGOU', `web-1 ${run}`);
await page.screenshot({ path: `${OUT}/chat-web.png` });
await browser.close();
console.log('WEB OK');
