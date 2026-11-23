// Chrome na sala de teste do site, conversando com o `bancada_soundboard` pela entrada e pela
// saída: corta e toca o som que o cliente subiu, aperta o "Parar", e espera ver o aviso do som
// que o cliente toca e ele sumir no "Parar" do cliente. Usa o puppeteer da bancada do spike.
import { createRequire } from 'node:module';
import { createInterface } from 'node:readline';
const require = createRequire(process.env.CONCORD_BANCADA + '/');
const puppeteer = require('puppeteer-core');

const { CONCORD_URL: base, CONCORD_ROOM: room, CONCORD_USER: user, CONCORD_PASS: pass, OUT, SOM_ID: id } = process.env;
const ordens = createInterface({ input: process.stdin })[Symbol.asyncIterator]();
const ordem = async (esperada) => {
  const { value } = await ordens.next();
  if (value !== esperada) throw new Error(`esperava "${esperada}", veio "${value}"`);
};

const browser = await puppeteer.launch({
  executablePath: '/opt/google/chrome/chrome',
  headless: 'new',
  userDataDir: `${OUT}/perfil-soundboard`,
  args: ['--use-fake-ui-for-media-stream', '--use-fake-device-for-media-stream', '--autoplay-policy=no-user-gesture-required', '--no-first-run', '--mute-audio'],
});
try {
  const page = await browser.newPage();
  await page.setViewport({ width: 1280, height: 800 });
  await page.goto(`${base}/login`, { waitUntil: 'domcontentloaded' });
  await page.evaluate(async (u, p) => {
    await fetch('/api/auth/login', { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ username: u, password: p }) });
  }, user, pass);
  // Corte pelo site, como nas configurações: o cliente tem de receber e respeitar.
  const corte = await page.evaluate(async (som) => {
    const r = await fetch(`/api/sounds/${som}`, { method: 'PATCH', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ trimStart: 0.5, trimEnd: 5.5 }) });
    return r.status;
  }, id);
  console.log('WEB CORTOU', corte);
  await page.goto(`${base}/rooms/${room}`, { waitUntil: 'domcontentloaded' });
  await page.waitForSelector('button[title="Soundboard"]', { timeout: 60000 });
  console.log('WEB NA SALA');

  await ordem('tocar');
  await page.click('button[title="Soundboard"]');
  const botao = 'button[title="Tocar bancada-tom pra todo mundo"]';
  await page.waitForSelector(botao, { timeout: 20000 });
  await page.click(botao);
  console.log('WEB TOCOU');

  await ordem('parar');
  await page.click('button[title^="Parar o som"]');
  console.log('WEB PAROU');

  // O som do cliente aparece com o nome dele, e some no "Parar" dele antes dos 2,5 s do aviso.
  await ordem('ouvir');
  const aviso = `${user} tocou bancada-tom`;
  await page.waitForFunction((t) => document.body.innerText.includes(t), { timeout: 20000 }, aviso);
  const viu = Date.now();
  console.log('WEB VIU o aviso do cliente');
  await page.screenshot({ path: `${OUT}/soundboard-web.png` });
  await page.waitForFunction((t) => !document.body.innerText.includes(t), { timeout: 10000 }, aviso);
  console.log('WEB AVISO SUMIU EM', Date.now() - viu, 'ms');
  console.log('WEB OK');
} finally {
  await browser.close();
}
