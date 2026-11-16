// Chrome na sala como um espectador do site: publica uma fala (`FAKE_WAV`), clica em
// "Assistir" só com `ASSISTE=1` e grava o `getStats` uma vez por segundo, por `DURACAO_MS`.
// `ESCONDE_MS` esconde e mostra o <video> nesse ritmo (o adaptiveStream do site para de
// pedir a camada). Usa o puppeteer da bancada do spike.
import { createRequire } from 'node:module';
const require = createRequire(process.env.CONCORD_BANCADA + '/');
const puppeteer = require('puppeteer-core');

const { CONCORD_URL: base, CONCORD_ROOM: room, CONCORD_USER: user, CONCORD_PASS: pass, OUT, FAKE_WAV } = process.env;
const duracao = Number(process.env.DURACAO_MS ?? 60000);
const esconde = Number(process.env.ESCONDE_MS ?? 0);
const espera = (ms) => new Promise((r) => setTimeout(r, ms));
const browser = await puppeteer.launch({
  executablePath: '/opt/google/chrome/chrome',
  headless: 'new',
  userDataDir: `${OUT}/perfil-stats`,
  args: ['--use-fake-ui-for-media-stream', '--use-fake-device-for-media-stream', '--autoplay-policy=no-user-gesture-required', '--no-first-run', '--mute-audio', ...(FAKE_WAV ? [`--use-file-for-fake-audio-capture=${FAKE_WAV}`] : [])],
});
const page = await browser.newPage();
await page.setViewport({ width: 1280, height: 760 });
// Guarda as conexões do site pra ler o `getStats` delas.
await page.evaluateOnNewDocument(() => {
  window.conexoes = [];
  const Original = window.RTCPeerConnection;
  window.RTCPeerConnection = class extends Original {
    constructor(...args) {
      super(...args);
      window.conexoes.push(this);
    }
  };
});
await page.goto(`${base}/login`, { waitUntil: 'domcontentloaded' });
await page.evaluate(async (u, p) => { await fetch('/api/auth/login', { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ username: u, password: p }) }); }, user, pass);
await page.goto(`${base}/rooms/${room}`, { waitUntil: 'domcontentloaded' });
await espera(7000);

// Acumulados do que chega: vídeo (o maior, se houver mais de um) e cada áudio.
const ler = () => page.evaluate(async () => {
  const out = { video: null, audios: [] };
  for (const pc of window.conexoes) {
    const stats = await pc.getStats();
    stats.forEach((r) => {
      if (r.type !== 'inbound-rtp') return;
      if (r.kind === 'video') {
        if (!out.video || (r.framesDecoded ?? 0) > out.video.decod) {
          out.video = { decod: r.framesDecoded ?? 0, fps: r.framesPerSecond ?? 0, w: r.frameWidth ?? 0, h: r.frameHeight ?? 0, kf: r.keyFramesDecoded ?? 0, freeze: r.freezeCount ?? 0, freezeS: r.totalFreezesDuration ?? 0, jb: r.jitterBufferDelay ?? 0, jbN: r.jitterBufferEmittedCount ?? 0, bytes: r.bytesReceived ?? 0 };
        }
      } else if (r.kind === 'audio') {
        out.audios.push({ id: r.trackIdentifier, oculto: r.concealedSamples ?? 0, total: r.totalSamplesReceived ?? 0 });
      }
    });
  }
  return out;
});

let clique = null;
let primeiro = null;
if (process.env.ASSISTE === '1') {
  const clicou = await page.evaluate(() => {
    const b = [...document.querySelectorAll('button')].find((x) => x.textContent.trim() === 'Assistir');
    b?.click();
    return !!b;
  });
  clique = Date.now();
  console.log('CLICOU', clicou);
  // Do clique ao primeiro quadro decodificado, em passos de 50 ms.
  while (Date.now() - clique < 15000) {
    if ((await ler()).video?.decod > 0) { primeiro = Date.now() - clique; break; }
    await espera(50);
  }
  console.log('PRIMEIRO_QUADRO_MS', primeiro ?? 'nenhum em 15 s');
}
if (esconde > 0) {
  await page.evaluate((ms) => {
    setInterval(() => document.querySelectorAll('video').forEach((v) => { v.style.display = v.style.display === 'none' ? '' : 'none'; }), ms);
  }, esconde);
}

// Uma linha por segundo com o que mudou desde a anterior.
const pct = (a, b) => (b > 0 ? ((100 * a) / b).toFixed(1) : '-');
let antes = await ler();
const inicio = Date.now();
let congelou = 0;
while (Date.now() - inicio < duracao) {
  await espera(1000);
  const agora = await ler();
  const v = agora.video, va = antes.video;
  let linha = `SEG ${Math.round((Date.now() - inicio) / 1000)}`;
  if (v) {
    const dJb = v.jb - (va?.jb ?? 0), dN = v.jbN - (va?.jbN ?? 0);
    linha += ` video ${v.w}x${v.h} ${v.fps} fps, ${Math.round(((v.bytes - (va?.bytes ?? 0)) * 8) / 1000)} kb/s, decod +${v.decod - (va?.decod ?? 0)}, keyframes +${v.kf - (va?.kf ?? 0)}, congelamentos +${v.freeze - (va?.freeze ?? 0)} (${(v.freezeS - (va?.freezeS ?? 0)).toFixed(2)} s), jitter buffer ${dN > 0 ? Math.round((1000 * dJb) / dN) : '-'} ms`;
    congelou = v.freeze;
  } else {
    linha += ' video nenhum';
  }
  for (const a of agora.audios) {
    const b = antes.audios.find((x) => x.id === a.id);
    linha += ` | audio ocultado ${pct(a.oculto - (b?.oculto ?? 0), a.total - (b?.total ?? 0))}%`;
  }
  console.log(linha);
  antes = agora;
}
console.log('FIM congelamentos', congelou, 'primeiro quadro', primeiro ?? '-', 'ms');
await page.screenshot({ path: `${OUT}/assiste-stats.png` });
await browser.close();
