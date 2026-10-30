'use strict';
// Catches help displacing real project work, hidden execution gates, and unsafe navigation.
// Actual authenticated HTTP/Chromium; no synthetic successful model state.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const [playwrightArg, chromiumArg, output] = process.argv.slice(2);
const { chromium } = require(path.dirname(playwrightArg));
(async () => {
  const input = JSON.parse(fs.readFileSync(0, 'utf8'));
  const browser = await chromium.launch({ headless: true, executablePath: chromiumArg });
  const results = [], errors = [], posts = [], measurements = [];
  try {
    const context = await browser.newContext({ viewport: { width: 1440, height: 1000 } });
    const page = await context.newPage();
    page.on('pageerror', error => errors.push(error.name));
    page.on('request', request => { if (request.method() === 'POST') posts.push({ path: new URL(request.url()).pathname, kind: request.postDataJSON()?.kind }); });
    async function check(name, run) {
      try { await run(); results.push({ name, passed: true }); }
      catch (error) { results.push({ name, passed: false, error: error.message.split('\n')[0] }); }
    }
    await page.goto(input.url);
    await page.waitForFunction(() => document.body.dataset.connected === 'true');
    await check('existing-project-selection-and-real-progress-are-before-long-help-on-both-screens', async () => {
      for (const width of [375, 1440]) {
        await page.setViewportSize({ width, height: width === 375 ? 812 : 1000 });
        await page.evaluate(() => scrollTo(0, 0));
        const selectBox = await page.locator('#project-select').boundingBox();
        assert.ok(selectBox.y + selectBox.height < 700, 'Existing projects must be accessible on the first screen');
        assert.equal(await page.locator('#my-projects').count(), 1, 'A visible My projects entry is required');
        const progress = page.locator('#current-progress');
        assert.equal(await progress.count(), 1, 'Current phase and next action must not be buried in execution logs');
        const box = await progress.boundingBox();
        assert.ok(box.y + box.height < 760, 'Authoritative progress and next action must fit the first mobile screen');
        const requirementTop = (await page.locator('#requirement').boundingBox()).y;
        measurements.push({ width, projectBottom: selectBox.y + selectBox.height, progressBottom: box.y + box.height, requirementTop,
          pageHeight: await page.evaluate(() => document.documentElement.scrollHeight) });
        await page.screenshot({ path: path.join(output, 'compact-' + width + '.png'), fullPage: true });
        assert.ok(requirementTop < 760, 'Requirement editing must start on the first screen, before detailed setup diagnostics; actual y=' + requirementTop);
        const gate = page.locator('#setup-link');
        assert.equal(await gate.count(), 1, 'Detailed setup needs an accessible entry near the execution restriction');
        assert.ok((await gate.boundingBox()).y < 700, 'The setup entry must remain on the first screen');
        assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true);
      }
    });
    await check('workflow-and-setup-instructions-expand-by-keyboard-without-hiding-gates', async () => {
      assert.equal(await page.locator('#workflow-map').isVisible(), false, 'Workflow explanation must be collapsed initially');
      assert.equal(await page.locator('#role-architect').isVisible(), false);
      assert.equal(await page.locator('#setup-install').isVisible(), false, 'Long setup instructions should be on demand');
      assert.equal(await page.locator('#setup-status').isVisible(), true);
      assert.equal(await page.locator('#setup-detail').isVisible(), true);
      assert.equal(await page.locator('#setup-recheck').isVisible(), true);
      const before = posts.length;
      await page.locator('#workflow-help > summary').focus();
      await page.keyboard.press('Enter');
      assert.equal(await page.locator('#workflow-map').isVisible(), true);
      assert.equal(await page.locator('#role-architect').isVisible(), true);
      await page.keyboard.press('Enter');
      assert.equal(await page.locator('#workflow-map').isVisible(), false);
      await page.locator('#setup-instructions > summary').focus();
      await page.keyboard.press('Enter');
      assert.equal(await page.locator('#setup-install').isVisible(), true);
      assert.equal(await page.locator('#shutdown-workbench').isVisible(), true);
      await page.keyboard.press('Enter');
      assert.equal(posts.length, before, 'Opening help must not submit any operation');
      if (input.linux) {
        assert.match(await page.locator('#setup-detail').innerText(), /Linux.*尚未验证/);
        assert.equal(await page.locator('#generate-document').isDisabled(), true);
      }
    });
    await check('project-list-search-keeps-selection-and-polling-keeps-keyboard-focus', async () => {
      const selected = await page.locator('body').getAttribute('data-project');
      assert.equal(await page.locator('#project-search').count(), 1);
      await page.locator('#my-projects-toggle').click();
      await page.locator('#project-search').fill('no-matching-project-unique-compact-probe');
      assert.match(await page.locator('#project-list').innerText(), /没有匹配/);
      assert.equal(await page.locator('body').getAttribute('data-project'), selected);
      await page.waitForTimeout(2800); // More than two normal polls must not recreate this input.
      assert.equal(await page.locator('#project-search').evaluate(element => element === document.activeElement), true);
      await page.locator('#project-search').fill('');
      const target = page.locator('#project-list button[data-project]').filter({ hasText: '210浏览器项目二' }).last();
      const id = await target.getAttribute('data-project');
      await target.focus(); await page.keyboard.press('Enter');
      await page.waitForFunction(value => document.body.dataset.project === value, id);
      assert.match(await page.locator('#current-project-name').innerText(), /210浏览器项目二/);
      assert.match(await page.locator('#progress-title').innerText(), /需求/);
      assert.match(await page.locator('#progress-next').innerText(), /需求/);
      assert.match(await page.locator('#project-select option:checked').innerText(), new RegExp(id.slice(0, 8)));
      await page.reload();
      await page.waitForFunction(() => document.body.dataset.connected === 'true');
      assert.equal(await page.locator('body').getAttribute('data-project'), id);
      assert.equal(await page.locator('#workflow-map').isVisible(), false);
      assert.equal(await page.locator('#generate-document').isDisabled(), input.linux);
    });
    await check('layout-and-navigation-never-submit-model-or-consent-actions', async () => {
      assert.deepEqual(errors, []);
      assert.deepEqual(posts.filter(row => row.path !== '/api/session'), []);
    });
  } finally {
    fs.writeFileSync(path.join(output, 'report.json'), JSON.stringify({ results, errors, posts, measurements,
      browserLocation: input.linux ? 'Windows client through SSH, not Linux server Chromium' : 'Windows client with real local HTTP' }, null, 2));
    console.log(JSON.stringify({ results, errors, measurements }));
    await browser.close();
  }
  if (results.some(row => !row.passed)) process.exitCode = 1;
})().catch(() => { console.error('Compact layout validation failed; see redacted report'); process.exitCode = 1; });
