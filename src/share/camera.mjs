// Chrome de câmera falsa na sala: liga, muta, volta e sai, pro `bancada_camera` medir o que
// chega no cliente. Usa o puppeteer da bancada do spike.
import { createRequire } from 'node:module';
const require = createRequire(process.env.CONCORD_BANCADA + '/');
const puppeteer = require('puppeteer-core');

const { CONCORD_URL: base, CONCORD_ROOM: room, CONCORD_USER: user, CONCORD_PASS: pass, OUT } = process.env;
const espera = (ms) => new Promise((r) => setTimeout(r, ms));
const browser = await puppeteer.launch({
  executablePath: '/opt/google/chrome/chrome',
  headless: 'new',
  userDataDir: `${OUT}/perfil-camera`,
  args: ['--use-fake-ui-for-media-stream', '--use-fake-device-for-media-stream', '--no-first-run', '--mute-audio'],
});
const page = await browser.newPage();
// Guarda as conexões, pra ler o que sai de vídeo.
await page.evaluateOnNewDocument(() => {
  const Original = window.RTCPeerConnection;
  window.__pcs = [];
  window.RTCPeerConnection = function (...args) {
    const pc = new Original(...args);
    window.__pcs.push(pc);
    return pc;
  };
  window.RTCPeerConnection.prototype = Original.prototype;
});
const enviado = () => page.evaluate(async () => {
  const out = [];
  for (const pc of window.__pcs) {
    (await pc.getStats()).forEach((s) => {
      if (s.type === 'outbound-rtp' && s.kind === 'video') out.push(`${s.rid ?? '-'} ${s.scalabilityMode ?? ''} ${s.frameWidth}x${s.frameHeight} ${s.framesPerSecond ?? 0}fps`);
    });
  }
  return out.join(' | ');
});
await page.goto(`${base}/login`, { waitUntil: 'domcontentloaded' });
await page.evaluate(async (u, p) => { await fetch('/api/auth/login', { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ username: u, password: p }) }); }, user, pass);
await page.goto(`${base}/rooms/${room}`, { waitUntil: 'domcontentloaded' });
await espera(8000);

// Ctrl+Shift+V é o atalho de câmera do site: liga, e depois muta sem despublicar.
const camera = async (fase) => {
  await page.keyboard.down('Control');
  await page.keyboard.down('Shift');
  await page.keyboard.press('KeyV');
  await page.keyboard.up('Shift');
  await page.keyboard.up('Control');
  console.log('CAMERA', fase);
};
await camera('LIGADA');
await espera(6000);
console.log('CAMERA ENVIA', await enviado());
await espera(8000);
await camera('MUTADA');
await espera(8000);
await camera('VOLTOU');
await espera(8000);
await page.screenshot({ path: `${OUT}/camera.png` });
// Sair navegando: o site desconecta no `pagehide`. Fechar direto deixa o servidor esperar o
// timeout, uns 20 s.
await page.goto('about:blank');
console.log('CAMERA SAIU');
await espera(2000);
await browser.close();
