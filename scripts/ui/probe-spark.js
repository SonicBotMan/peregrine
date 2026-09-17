// reseed (reuse shot-v5's scene), wait for samples, verify sparkline
const { chromium } = require('playwright');
const http = require('http');
const rest = (method, path, body) =>
  new Promise((resolve, reject) => {
    const req = http.request(
      `http://127.0.0.1:8420${path}`,
      { method, headers: body ? { 'content-type': 'application/json' } : {} },
      (res) => {
        let d = '';
        res.on('data', (c) => (d += c));
        res.on('end', () => resolve(d ? JSON.parse(d) : null));
      },
    );
    req.on('error', reject);
    if (body) req.write(JSON.stringify(body));
    req.end();
  });
const wait = (ms) => new Promise((r) => setTimeout(r, ms));
(async () => {
  for (const t of await rest('GET', '/tasks')) await rest('DELETE', `/tasks/${t.id}`);
  await wait(600);
  const r = await rest('POST', '/tasks', { url: 'http://localhost:8931/big.bin', save_path: '/tmp/dlshot/v5-running.bin' });
  await rest('PUT', `/tasks/${r.id}/limit`, { bps: 60000 });
  await rest('POST', '/tasks', { url: 'http://localhost:8931/r2.bin', save_path: '/tmp/dlshot/v5-done.bin' });
  await rest('POST', '/tasks', { url: 'http://localhost:8931/peregrine-proposal-v2.pdf', save_path: '/tmp/dlshot/v5-doc.pdf' });
  await wait(1500);
  await rest('POST', '/tasks', { url: 'http://localhost:9999/v5-nope.bin', save_path: '/tmp/dlshot/v5-fail.bin' });
  await wait(12000); // let the running task accumulate speed samples

  const browser = await chromium.launch({ headless: true });
  const page = await browser.newPage({ viewport: { width: 1180, height: 720 }, deviceScaleFactor: 2 });
  await page.goto('http://localhost:5202/', { waitUntil: 'networkidle' });
  const running = page.locator(".row[data-status='running']").first();
  await running.waitFor({ timeout: 10000 });
  await running.click();
  await wait(400);
  await page.locator('.dtab', { hasText: 'Speed graph' }).click();
  await wait(400);
  const pts = await page.locator('.drawer polyline.line').first().getAttribute('points').catch(() => null);
  const hint = await page.locator('.drawer .hint').textContent().catch(() => null);
  await page.screenshot({ path: '/tmp/v5b-spark.png' });
  console.log('sparkline-points:', pts ? `${pts.slice(0, 50)}… (${pts.split(' ').length} pts)` : null, '| hint:', hint);
  await browser.close();
})().catch((e) => { console.error('FATAL', e.message); process.exit(1); });
