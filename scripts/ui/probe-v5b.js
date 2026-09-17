// V5-b probe: j/k walk, glyph, errsum, light failed hairline, sort carets
const { chromium } = require('playwright');
const BASE = 'http://localhost:5202';
const D = 'http://127.0.0.1:8420';
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
(async () => {
  const b = await chromium.launch();
  const p = await b.newPage({ viewport: { width: 1180, height: 720 }, deviceScaleFactor: 2 });
  const errs = [];
  p.on('pageerror', (e) => errs.push(String(e).slice(0, 120)));
  await p.goto(BASE, { waitUntil: 'networkidle' });
  await p.locator('.row').first().waitFor({ timeout: 10000 });

  const out = {};

  // 1. file-type glyphs present + letters
  out.glyphs = await p.locator('.ftype').evaluateAll((els) =>
    els.slice(0, 8).map((e) => e.textContent.trim()),
  );

  // 2. failed row shows errsum text
  out.errsum = await p
    .locator(".row[data-status='failed'] .errsum")
    .first()
    .textContent()
    .catch(() => null);

  // 3. j/k selection walk
  await p.locator('body').press('j');
  await sleep(150);
  const sel1 = await p.evaluate(() => document.querySelector('.row.selected')?.dataset.id ?? null);
  await p.locator('body').press('j');
  await sleep(150);
  const sel2 = await p.evaluate(() => document.querySelector('.row.selected')?.dataset.id ?? null);
  await p.locator('body').press('k');
  await sleep(150);
  const sel3 = await p.evaluate(() => document.querySelector('.row.selected')?.dataset.id ?? null);
  out.jk = { first: sel1, moved: sel2, back: sel3, ok: sel1 !== null && sel2 !== sel1 && sel3 === sel1 };

  // 4. drawer opens on click, sparkline + info tab
  await p.locator('.row').last().click();
  await sleep(500);
  out.drawer = await p.locator('.drawer').count() > 0;
  out.spark = await p.locator('.drawer svg polyline, .drawer canvas').count() > 0;
  out.tabs = await p.locator('.dtab').evaluateAll((els) => els.map((e) => e.textContent.trim()));

  // 5. sort caret appears after clicking Size header
  out.caretBefore = await p.locator('.thead .caret').count();
  await p.locator('button.th', { hasText: 'Size' }).click();
  await sleep(200);
  out.caretAfter = await p.locator('.thead .caret').count();

  // 6. light theme: failed hairline + no pink row
  await p.locator('.ghost').last().click(); // theme toggle in statusbar
  await sleep(400);
  out.lightTheme = await p.evaluate(() => document.documentElement.dataset.theme);
  out.lightFailed = await p.evaluate(() => {
    const r = document.querySelector(".row[data-status='failed']");
    return r ? getComputedStyle(r).background : 'no-failed-row';
  });
  await p.screenshot({ path: '/tmp/v5b-light.png' });
  await p.locator('.ghost').last().click(); // back to dark
  await sleep(300);
  await p.screenshot({ path: '/tmp/v5b-dark.png' });

  out.pageerrors = errs;
  console.log(JSON.stringify(out, null, 1));
  await b.close();
})().catch((e) => {
  console.error('FATAL', e.message);
  process.exit(1);
});
