// Chrome na sala, antes e depois de clicar em "Assistir": mede cada tom em cada lado de
// cada <audio>. Quem chama é o `bancada_tela_audio`. Usa o puppeteer da bancada do spike.
import { createRequire } from 'node:module';
const require = createRequire(process.env.CONCORD_BANCADA + '/');
const puppeteer = require('puppeteer-core');

const { CONCORD_URL: base, CONCORD_ROOM: room, CONCORD_USER: user, CONCORD_PASS: pass, OUT } = process.env;
const browser = await puppeteer.launch({
  executablePath: '/opt/google/chrome/chrome',
  headless: 'new',
  userDataDir: `${OUT}/perfil-som`,
  args: ['--use-fake-ui-for-media-stream', '--use-fake-device-for-media-stream', '--autoplay-policy=no-user-gesture-required', '--no-first-run', '--mute-audio'],
});
const page = await browser.newPage();
await page.goto(`${base}/login`, { waitUntil: 'domcontentloaded' });
await page.evaluate(async (u, p) => { await fetch('/api/auth/login', { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ username: u, password: p }) }); }, user, pass);
await page.goto(`${base}/rooms/${room}`, { waitUntil: 'domcontentloaded' });
await new Promise((r) => setTimeout(r, 8000));

const medir = () => page.evaluate(async () => {
  const ctx = new AudioContext({ sampleRate: 48000 });
  await ctx.resume();
  const hz = [440, 1000, 330, 770, 660];
  const tom = (x, f) => {
    let s = 0, c = 0;
    for (let i = 0; i < x.length; i++) { const a = (2 * Math.PI * f * i) / 48000; s += x[i] * Math.sin(a); c += x[i] * Math.cos(a); }
    return Math.round((32768 * Math.SQRT2 * Math.hypot(s, c)) / x.length);
  };
  const out = [];
  for (const el of document.querySelectorAll('audio')) {
    const tracks = el.srcObject?.getAudioTracks?.() ?? [];
    if (!tracks.length) continue;
    const src = ctx.createMediaStreamSource(new MediaStream(tracks));
    const proc = ctx.createScriptProcessor(4096, 2, 2);
    const L = [], R = [];
    proc.onaudioprocess = (e) => { L.push(...e.inputBuffer.getChannelData(0)); R.push(...e.inputBuffer.getChannelData(1)); };
    src.connect(proc);
    proc.connect(ctx.destination);
    await new Promise((r) => setTimeout(r, 1500));
    src.disconnect();
    proc.disconnect();
    out.push({ mudo: el.muted, volume: el.volume, L: hz.map((f) => `${f}=${tom(L, f)}`).join(' '), R: hz.map((f) => `${f}=${tom(R, f)}`).join(' ') });
  }
  await ctx.close();
  return out;
});

console.log('ANTES', JSON.stringify(await medir()));
const clicou = await page.evaluate(() => {
  const b = [...document.querySelectorAll('button')].find((x) => x.textContent.trim() === 'Assistir');
  b?.click();
  return !!b;
});
console.log('CLICOU', clicou);
await new Promise((r) => setTimeout(r, 6000));
console.log('DEPOIS', JSON.stringify(await medir()));
await page.screenshot({ path: `${OUT}/ouve-tela.png` });
await browser.close();
