// Chrome na sala como um espectador do site: publica uma fala (`FAKE_WAV`), clica em
// "Assistir" só com `ASSISTE=1` e grava o `getStats` uma vez por segundo, por `DURACAO_MS`.
// `ESCONDE_MS` esconde e mostra o <video> nesse ritmo (o adaptiveStream do site para de
// pedir a camada). `PASSO_MS` muda o intervalo das linhas (padrão 1000). Cada linha leva a
// hora do relógio, pra casar com o log de quem envia. Usa o puppeteer da bancada do spike.
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
          out.video = { decod: r.framesDecoded ?? 0, fps: r.framesPerSecond ?? 0, w: r.frameWidth ?? 0, h: r.frameHeight ?? 0, kf: r.keyFramesDecoded ?? 0, freeze: r.freezeCount ?? 0, freezeS: r.totalFreezesDuration ?? 0, jb: r.jitterBufferDelay ?? 0, jbN: r.jitterBufferEmittedCount ?? 0, bytes: r.bytesReceived ?? 0,
            recebidos: r.framesReceived ?? 0, pacotes: r.packetsReceived ?? 0, perdidos: r.packetsLost ?? 0, rtx: r.retransmittedPacketsReceived ?? 0, rtxSsrc: r.rtxSsrc ?? 0, nack: r.nackCount ?? 0, pli: r.pliCount ?? 0, fir: r.firCount ?? 0 };
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
  // O que o SFU ofereceu pro vídeo que chega: H.264, o RTX dele e o feedback de cada um.
  console.log('SDP', await page.evaluate(() => window.conexoes.map((pc) => {
    const sdp = pc.remoteDescription?.sdp ?? '';
    const h264 = [...sdp.matchAll(/a=rtpmap:(\d+) H264/g)].map((m) => m[1]);
    const rtx = [...sdp.matchAll(/a=fmtp:(\d+) apt=(\d+)/g)].filter((m) => h264.includes(m[2])).map((m) => `${m[1]}->${m[2]}`);
    const fb = h264.map((pt) => `${pt}:${[...sdp.matchAll(new RegExp(`a=rtcp-fb:${pt} ([^\\r\\n]+)`, 'g'))].map((m) => m[1]).join('/')}`);
    return h264.length ? `H264 [${fb.join(' ')}] rtx [${rtx.join(' ')}]` : '-';
  }).join(' | ')));
}
if (esconde > 0) {
  await page.evaluate((ms) => {
    setInterval(() => document.querySelectorAll('video').forEach((v) => { v.style.display = v.style.display === 'none' ? '' : 'none'; }), ms);
  }, esconde);
}

// Uma linha por passo com o que mudou desde a anterior. `perdidos` é o acumulado do
// receptor: sobe com o buraco e desce quando o reenvio chega.
const passo = Number(process.env.PASSO_MS ?? 1000);
const hora = () => new Date().toTimeString().slice(0, 8) + '.' + String(Date.now() % 1000).padStart(3, '0');
const pct = (a, b) => (b > 0 ? ((100 * a) / b).toFixed(1) : '-');
let antes = await ler();
const inicio = Date.now();
let congelou = 0;
let viuRtx = false;
let tAntes = inicio;
while (Date.now() - inicio < duracao) {
  await espera(passo);
  const agora = await ler();
  const t = Date.now(), dt = (t - tAntes) / 1000;
  tAntes = t;
  const v = agora.video, va = antes.video;
  let linha = `SEG ${+((t - inicio) / 1000).toFixed(passo < 1000 ? 1 : 0)} ${hora()}`;
  if (v) {
    const d = (k) => v[k] - (va?.[k] ?? 0);
    const dJb = d('jb'), dN = d('jbN');
    linha += ` video ${v.w}x${v.h} ${v.fps} fps, ${Math.round((d('bytes') * 8) / 1000 / dt)} kb/s, decod +${d('decod')}, keyframes +${d('kf')}, congelamentos +${d('freeze')} (${d('freezeS').toFixed(2)} s), jitter buffer ${dN > 0 ? Math.round((1000 * dJb) / dN) : '-'} ms`;
    linha += `, recebidos +${d('recebidos')}, pacotes +${d('pacotes')}, perdidos ${v.perdidos} (${d('perdidos') >= 0 ? '+' : ''}${d('perdidos')}), reenviados +${d('rtx')}, NACK +${d('nack')}, PLI +${d('pli')}, FIR +${d('fir')}`;
    if (!viuRtx) { console.log('RTX_SSRC', v.rtxSsrc || 'nenhum'); viuRtx = true; }
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
