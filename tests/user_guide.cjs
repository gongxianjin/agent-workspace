'use strict';
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const { pathToFileURL } = require('node:url');
const [playwrightPath, executablePath, output, guideFile] = process.argv.slice(2);
const { chromium } = require(path.dirname(playwrightPath));

(async () => {
  fs.mkdirSync(output, { recursive: true });
  const browser = await chromium.launch({ executablePath, headless: true });
  const results = [], errors = [], requests = [];
  try {
    const page = await browser.newPage();
    page.on('pageerror', error => errors.push(String(error)));
    await page.route('**/*', route => {
      if (route.request().url().startsWith('file:')) return route.continue();
      requests.push(route.request().url());
      return route.abort();
    });
    for (const width of [1440, 375]) {
      await page.setViewportSize({ width, height: 900 });
      await page.goto(pathToFileURL(path.resolve(guideFile || path.resolve(__dirname, '../USER-GUIDE.html'))).href);
      await page.locator('h1').waitFor();
      assert.match(await page.title(), /安装与使用手册/);
      assert.equal(await page.locator('section').count(), 12);
      const anchors = await page.locator('nav a').evaluateAll(nodes => nodes.map(node => node.hash.slice(1)));
      assert.equal(new Set(anchors).size, 12);
      for (const id of anchors) assert.equal(await page.locator('[id="' + id + '"]').count(), 1);
      assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true, 'No horizontal page overflow at ' + width);
      const first = page.locator('nav a').first();
      await first.focus();
      await page.keyboard.press('Enter');
      assert.equal(new URL(page.url()).hash, '#quick');
      await page.locator('#troubleshooting').scrollIntoViewIfNeeded();
      assert.match(await page.locator('#troubleshooting').innerText(), /path_too_long/);
      assert.match(await page.locator('#handoff').innerText(), /干净ZIP/);
      assert.match(await page.locator('#checklist').innerText(), /尚未验收/);
      await page.screenshot({ path: path.join(output, 'guide-' + width + '.png'), fullPage: true });
      results.push({ name: 'offline-guide-navigation-and-layout-' + width, passed: true });
    }
    assert.deepEqual(requests, [], 'Reading the guide must not fetch remote assets');
    assert.deepEqual(errors, []);
    results.push({ name: 'no-page-errors-or-network-assets', passed: true });
    fs.writeFileSync(path.join(output, 'results.json'), JSON.stringify(results, null, 2));
    console.log(JSON.stringify(results));
  } finally {
    await browser.close();
  }
})().catch(error => { console.error(error); process.exitCode = 1; });
