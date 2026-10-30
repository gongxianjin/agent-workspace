'use strict';
// Only this trusted program executes browser actions; specification strings are never eval'd.
const fs = require('node:fs');
const path = require('node:path');
const http = require('node:http');
const assert = require('node:assert/strict');
const [rootArg, specArg, outputArg, profile = 'generic', playwrightArg, chromiumArg] = process.argv.slice(2);
if (!playwrightArg || !chromiumArg || !path.isAbsolute(playwrightArg) || !path.isAbsolute(chromiumArg)) {
  throw Error('Explicit trusted browser dependencies are required');
}
const { chromium } = require(path.dirname(playwrightArg));
const root = fs.realpathSync(rootArg), output = path.resolve(outputArg);
const spec = JSON.parse(fs.readFileSync(specArg, 'utf8'));
if (profile === 'liuwa-v1') {
  // Fixed sample contract is selected by the approved document, not authored by the tester.
  const card = '[data-testid="place-card"]', search = '[data-testid="search"]';
  spec.cases.push(
    { name: 'fixed: six unverified examples and search', steps: [
      { op: 'count', selector: card, value: 6 }, { op: 'text', selector: '[data-testid="data-warning"]', value: '未核实' },
      { op: 'fill', selector: search, value: '中山' }, { op: 'count', selector: card, value: 1 },
      { op: 'text', selector: card, value: '中山公园' }, { op: 'fill', selector: search, value: '不存在地点xyz' },
      { op: 'count', selector: card, value: 0 }, { op: 'visible', selector: '[data-testid="empty-state"]' }] },
    { name: 'fixed: category filter', steps: [
      { op: 'click', selector: '[data-category="park"]' }, { op: 'count', selector: card, value: 2 },
      { op: 'click', selector: '[data-category="museum"]' }, { op: 'count', selector: card, value: 2 },
      { op: 'click', selector: '[data-category="all"]' }, { op: 'count', selector: card, value: 6 }] },
    { name: 'fixed: favorites persist across reload', steps: [
      { op: 'click', selector: '[data-place-id="p1"] [data-action="favorite"]' }, { op: 'reload' },
      { op: 'click', selector: '[data-action="favorites-only"]' }, { op: 'count', selector: card, value: 1 },
      { op: 'text', selector: card, value: '中山公园' }] },
    { name: 'fixed: storage failure remains usable', steps: [
      { op: 'storageFailure' }, { op: 'click', selector: '[data-place-id="p1"] [data-action="favorite"]' },
      { op: 'click', selector: '[data-action="favorites-only"]' }, { op: 'count', selector: card, value: 1 },
      { op: 'noErrors' }, { op: 'noOverflow' }] }
  );
}
const report = { assertions: 0, cases: [] };
const csp = "default-src 'none'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; font-src 'self'; connect-src 'none'; frame-src 'none'; object-src 'none'; base-uri 'none'; form-action 'none'";
const mime = { '.html': 'text/html', '.css': 'text/css', '.js': 'text/javascript', '.json': 'application/json', '.svg': 'image/svg+xml' };
const server = http.createServer((req, res) => {
  try {
    if (req.method !== 'GET' || req.headers.host !== `127.0.0.1:${server.address().port}`) throw Error('Denied');
    const relative = decodeURIComponent(new URL(req.url, 'http://127.0.0.1').pathname).replace(/^\//, '') || 'index.html';
    if (relative.split('/').some(s => !s || s.startsWith('.') || /[:\\]/.test(s))) throw Error('Invalid path');
    const target = fs.realpathSync(path.join(root, relative));
    if (!target.startsWith(root + path.sep) || !fs.statSync(target).isFile()) throw Error('Outside snapshot');
    const raw = fs.readFileSync(target);
    res.writeHead(200, { 'Content-Type': `${mime[path.extname(target)] || 'application/octet-stream'}; charset=utf-8`,
      'Content-Length': raw.length, 'Content-Security-Policy': csp, 'X-Content-Type-Options': 'nosniff', 'Cache-Control': 'no-store' });
    res.end(raw);
  } catch { res.writeHead(404); res.end('Not found'); }
});

(async () => {
  let browser;
  try {
    await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
    const origin = `http://127.0.0.1:${server.address().port}`;
    browser = await chromium.launch({ headless: true, executablePath: chromiumArg,
      ...(process.platform === 'linux' ? { chromiumSandbox: true } : {}) });
    for (let index = 0; index < spec.cases.length; index++) {
      const test = spec.cases[index], errors = [], blocked = [];
      const context = await browser.newContext({ viewport: { width: 375, height: 812 }, serviceWorkers: 'block' });
      context.setDefaultTimeout(4000);
      await context.route('**/*', route => {
        const url = route.request().url();
        if (url.startsWith(origin + '/')) return route.continue();
        blocked.push(url); return route.abort();
      });
      await context.tracing.start({ screenshots: true, snapshots: true });
      const page = await context.newPage();
      page.on('pageerror', error => errors.push(String(error)));
      const result = { name: test.name, passed: false, assertions: 0 };
      const verify = run => { report.assertions++; result.assertions++; run(); };
      try {
        await page.goto(origin, { waitUntil: 'load' });
        for (const step of test.steps) {
          const locator = step.selector ? page.locator(step.selector) : null;
          switch (step.op) {
            case 'fill': await locator.fill(step.value); break;
            case 'click': await locator.click(); break;
            case 'reload': await page.reload({ waitUntil: 'load' }); break;
            case 'viewport': await page.setViewportSize({ width: step.width, height: step.height }); break;
            case 'storageFailure':
              await context.addInitScript(() => { Storage.prototype.setItem = () => { throw Error('storage disabled by authorized test'); }; });
              await page.reload({ waitUntil: 'load' }); break;
            case 'text': { const value = await locator.innerText(); verify(() => assert.ok(value.includes(step.value), `Text missing: ${step.value}`)); break; }
            case 'value': { const value = await locator.inputValue(); verify(() => assert.equal(value, step.value)); break; }
            case 'count': { const value = await locator.count(); verify(() => assert.equal(value, step.value)); break; }
            case 'visible': { const value = await locator.isVisible(); verify(() => assert.ok(value, `Not visible: ${step.selector}`)); break; }
            case 'noOverflow': { const value = await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth); verify(() => assert.ok(value, 'horizontal overflow')); break; }
            case 'noErrors': verify(() => assert.deepEqual(errors, [])); break;
            default: throw Error('Unknown operation');
          }
        }
        await page.setViewportSize({ width: 375, height: 812 });
        const fits = await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth);
        verify(() => assert.ok(fits, 'horizontal overflow at 375px (fixed probe)'));
        verify(() => assert.deepEqual(errors, [], 'Uncaught page error'));
        verify(() => assert.deepEqual(blocked, [], 'Unexpected external request'));
        result.passed = true;
      } catch (error) { result.error = String(error); }
      finally {
        await page.screenshot({ path: path.join(output, `case-${index + 1}.png`), fullPage: true }).catch(() => {});
        await context.tracing.stop({ path: path.join(output, `case-${index + 1}.zip`) });
        await context.close(); report.cases.push(result);
      }
    }
  } catch (error) { report.cases.push({ name: 'runner', passed: false, error: String(error) }); }
  finally {
    if (browser) await browser.close();
    await new Promise(resolve => server.close(resolve));
    fs.writeFileSync(path.join(output, 'report.json'), JSON.stringify(report, null, 2));
  }
  console.log(JSON.stringify(report));
  process.exitCode = report.assertions > 0 && report.cases.length > 0 && report.cases.every(row => row.passed) ? 0 : 1;
})();
