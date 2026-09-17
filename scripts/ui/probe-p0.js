// P0 end-to-end: delete-undo, empty-state paste-add, clipboard watch.
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

let pass = 0, fail = 0;
const ok = (name, cond) => { cond ? pass++ : fail++; console.log(`${cond ? 'PASS' : 'FAIL'} ${name}`); };

(async () => {
  // ensure at least one task exists (the 1st section needs a row)
  if ((await rest('GET', '/tasks')).length === 0) {
    await rest('POST', '/tasks', {
      url: 'http://localhost:8931/r2.bin',
      save_path: `/tmp/dltest/p0-seed-${Date.now()}.bin`,
    });
  }
  const browser = await chromium.launch({ headless: true });
  const context = await browser.newContext({ permissions: ['clipboard-read', 'clipboard-write'] });
  const page = await context.newPage();
  page.on('pageerror', (e) => console.log('PAGE-ERR:', String(e).slice(0, 200)));
  await page.goto('http://localhost:5202/', { waitUntil: 'networkidle' });
  await page.locator('.row').first().waitFor({ timeout: 8000 });

  // ---- 1. delete + undo (keyboard path) ----
  const before = await page.locator('.row').count();
  const firstId = await page.locator('.row').first().getAttribute('data-id');
  await page.locator('.row').first().click(); // select
  await page.keyboard.press('Delete');
  await page.waitForTimeout(300);
  const toastUndo = await page.locator('.toast .undo').count();
  ok('1a delete shows undo toast', toastUndo === 1);
  const rowsAfterDel = await page.locator('.row').count();
  ok('1b row hidden immediately', rowsAfterDel === before - 1);
  await page.locator('.toast .undo').click();
  await page.waitForTimeout(300);
  const rowsAfterUndo = await page.locator('.row').count();
  ok('1c undo restores row', rowsAfterUndo === before);
  const daemonStill = await rest('GET', `/tasks`).then((ts) => ts.some((t) => t.id === firstId));
  ok('1d daemon never deleted', daemonStill);

  // ---- 2. empty-state paste-and-go ----
  // empty the daemon for this section
  const all = await rest('GET', '/tasks');
  for (const t of all) await rest('DELETE', `/tasks/${t.id}`);
  await page.waitForFunction(() => document.querySelectorAll('.row').length === 0, { timeout: 8000 });
  const quickVisible = await page.locator('.quick-input').count();
  ok('2a empty state shows quick input', quickVisible === 1);
  // invalid URL first (list still empty → quick form visible)
  await page.locator('.quick-input').fill('not-a-url');
  await page.locator('.quick button[type="submit"]').click();
  await page.waitForTimeout(300);
  const errCount = await page.locator('.quick-err').count();
  const rowsBefore = await page.locator('.row').count();
  ok('2d invalid URL inline error', errCount === 1 && rowsBefore === 0);

  // then the happy path
  await page.locator('.quick-input').fill('http://localhost:8931/r2.bin');
  await page.locator('.quick button[type="submit"]').click();
  await page.waitForFunction(() => document.querySelectorAll('.row').length === 1, { timeout: 8000 });
  const quickTask = await rest('GET', '/tasks').then((ts) => ts[0]);
  ok('2b quick add creates task', !!quickTask && quickTask.url.includes('r2.bin'));
  ok('2c default dir applied', quickTask.save_path.includes('Downloads'));

  // ---- 3. clipboard watch ----
  // list is non-empty now → clip-banner path
  await page.evaluate(() => navigator.clipboard.writeText('http://localhost:8931/r2big.bin'));
  await page.evaluate(() => window.dispatchEvent(new Event('focus'))); // playwright focus event
  await page.waitForTimeout(500);
  const clipCount = await page.locator('.clip-banner').count();
  ok('3a clipboard offer appears', clipCount === 1);
  const clipText = await page.locator('.clip-url').textContent();
  ok('3b offer shows filename', clipText.includes('r2big.bin'));
  await page.locator('.clip-banner .ctl.primary').click();
  await page.waitForFunction(() => document.querySelectorAll('.row').length === 2, { timeout: 8000 });
  const tasks = await rest('GET', '/tasks');
  ok('3c accept downloads task', tasks.some((t) => t.url.includes('r2big.bin')));
  // re-focus: no re-offer (already tracked)
  await page.evaluate(() => window.dispatchEvent(new Event('focus')));
  await page.waitForTimeout(400);
  ok('3d no re-offer for tracked url', (await page.locator('.clip-banner').count()) === 0);

  // cleanup: leave one running capped task for the demo scene
  for (const t of await rest('GET', '/tasks')) await rest('DELETE', `/tasks/${t.id}`);
  const i = await rest('POST', '/tasks', {
    url: 'http://localhost:8931/model-weights-snapshot-v3.tar',
    save_path: '/tmp/dlshot/model-weights-snapshot-v3.tar',
  });
  await rest('PUT', `/tasks/${i.id}/limit`, { bps: 300000 });
  await browser.close();
  console.log(`\n${pass}/${pass + fail}`);
  process.exit(fail ? 1 : 0);
})().catch((e) => { console.error('FATAL', e.message); process.exit(1); });
