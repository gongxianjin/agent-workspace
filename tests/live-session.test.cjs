const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const modulePath = path.join(__dirname, '../live-session.js');
const session = fs.existsSync(modulePath) ? require(modulePath) : {};

// A missing deadline, retrying POST, or stale storage read must fail these contracts.
function clock(t) {
  let now = 0, id = 0;
  const timers = new Map();
  t.mock.method(globalThis, 'setTimeout', (callback, delay = 0) => {
    timers.set(++id, { callback, at: now + delay }); return id;
  });
  t.mock.method(globalThis, 'clearTimeout', key => timers.delete(key));
  async function flush() { for (let n = 0; n < 20; n++) await Promise.resolve(); }
  return {
    timers, flush,
    async tick(ms) {
      const until = now + ms;
      for (;;) {
        const next = [...timers].filter(([, value]) => value.at <= until).sort((a, b) => a[1].at - b[1].at)[0];
        if (!next) break;
        now = next[1].at; timers.delete(next[0]); next[1].callback(); await flush();
      }
      now = until; await flush();
    }
  };
}
const deferred = () => { let resolve, reject; const promise = new Promise((yes, no) => { resolve = yes; reject = no; }); return { promise, resolve, reject }; };
const response = (body, status = 200) => ({ ok: status >= 200 && status < 300, status, json: async () => body });
const observe = promise => promise.then(value => ({ value }), error => ({ error }));

test('deadline_bounds_fetch_and_body_read', async t => {
  for (const hanging of ['fetch', 'json', 'blob']) {
    await t.test(hanging, async t => {
      const time = clock(t); let calls = 0, signal, settled = false;
      const api = session.createApi({ getToken: () => 'private-test-token', fetchImpl: async (url, options) => {
        calls++; signal = options.signal;
        if (hanging === 'fetch') return new Promise(() => {});
        return { ok: true, status: 200, [hanging]: () => new Promise(() => {}) };
      } });
      const pending = observe(api(hanging === 'blob' ? '/api/document?project=x' : '/api/state', undefined, hanging === 'blob'));
      pending.then(() => { settled = true; });
      await time.flush(); await time.tick(hanging === 'blob' ? 29999 : 9999);
      assert.equal(settled, false);
      await time.tick(1);
      const { error } = await pending;
      assert.equal(error.category, 'timeout'); assert.equal(error.definitiveRejected, false);
      assert.equal(signal.aborted, true); assert.equal(calls, 1); assert.equal(time.timers.size, 0);
    });
  }
});

test('deadline_uses_long_recheck_and_short_session_and_receipt_bounds', async t => {
  for (const [url, value, deadline] of [
    ['/api/setup/recheck', {}, 120000], ['/api/session', { bootstrap: 'test' }, 10000],
    ['/api/setup', undefined, 10000], ['/api/telemetry?project=x', undefined, 10000],
    ['/api/receipts?scope=create&command=x', undefined, 10000], ['/api/command', {}, 30000]
  ]) {
    await t.test(url, async t => {
      const time = clock(t); let settled = false;
      const api = session.createApi({ getToken: () => '', fetchImpl: () => new Promise(() => {}) });
      const pending = observe(api(url, value)); pending.then(() => { settled = true; });
      await time.tick(deadline - 1); assert.equal(settled, false);
      await time.tick(1); assert.equal((await pending).error.category, 'timeout'); assert.equal(time.timers.size, 0);
    });
  }
});

test('external_abort_cleans_listener_and_never_claims_rejection', async t => {
  const time = clock(t), controller = new AbortController(); let adds = 0, removes = 0, signal;
  const add = controller.signal.addEventListener.bind(controller.signal), remove = controller.signal.removeEventListener.bind(controller.signal);
  t.mock.method(controller.signal, 'addEventListener', (...args) => { adds++; return add(...args); });
  t.mock.method(controller.signal, 'removeEventListener', (...args) => { removes++; return remove(...args); });
  const api = session.createApi({ getToken: () => '', fetchImpl: (url, options) => { signal = options.signal; return new Promise(() => {}); } });
  const pending = observe(api('/api/command', {}, false, { signal: controller.signal }));
  await time.flush(); controller.abort();
  const { error } = await pending;
  assert.equal(error.category, 'aborted'); assert.equal(error.name, 'AbortError'); assert.equal(error.definitiveRejected, false);
  assert.equal(signal.aborted, true); assert.equal(adds, 1); assert.equal(removes, 1); assert.equal(time.timers.size, 0);
  let calls = 0;
  const preAborted = session.createApi({ getToken: () => '', fetchImpl: () => { calls++; } });
  assert.equal((await observe(preAborted('/api/state', undefined, false, { signal: controller.signal }))).error.category, 'aborted');
  assert.equal(calls, 0);
});

test('success_cleans_timer_and_signal_and_uses_current_token_without_credentials', async t => {
  const time = clock(t), controller = new AbortController(); let options, token = 'first', removals = 0;
  const remove = controller.signal.removeEventListener.bind(controller.signal);
  t.mock.method(controller.signal, 'removeEventListener', (...args) => { removals++; return remove(...args); });
  const api = session.createApi({ getToken: () => token, fetchImpl: async (url, value) => { options = value; return response({ accepted: true }); } });
  token = 'second';
  assert.deepEqual(await api('/api/command', { kind: 'edit' }, false, { signal: controller.signal }), { accepted: true });
  assert.equal(options.method, 'POST'); assert.equal(options.headers.Authorization, 'Bearer second');
  assert.equal(options.credentials, 'omit'); assert.equal(options.cache, 'no-store');
  assert.equal(options.body, '{"kind":"edit"}'); assert.equal(removals, 1); assert.equal(time.timers.size, 0);
});

test('post_is_sent_once_and_only_explicit_precommit_4xx_is_definitive', async t => {
  for (const [status, body, definitive] of [[409, { error: 'CAS', outcome: 'rejected' }, true],
    [403, { error: 'expired', outcome: 'rejected' }, true], [409, { error: 'old backend' }, false],
    [500, { error: 'committed', outcome: 'unknown' }, false], [500, { outcome: 'rejected' }, false]]) {
    await t.test(String(status) + JSON.stringify(body), async t => {
      const time = clock(t); let calls = 0;
      const api = session.createApi({ getToken: () => '', fetchImpl: async () => { calls++; return response(body, status); } });
      const { error } = await observe(api('/api/command', {}));
      assert.equal(error.category, 'http'); assert.equal(error.status, status); assert.equal(error.definitiveRejected, definitive);
      await time.tick(200000); assert.equal(calls, 1); assert.equal(time.timers.size, 0);
    });
  }
});

test('transport_and_parse_failures_are_unknown_and_do_not_retry', async t => {
  for (const [kind, fetchImpl] of [
    ['transport', async () => { throw new TypeError('connection lost'); }],
    ['parse', async () => ({ ok: true, status: 200, json: async () => { throw new SyntaxError('bad body'); } })]
  ]) {
    await t.test(kind, async t => {
      const time = clock(t); let calls = 0;
      const api = session.createApi({ getToken: () => '', fetchImpl: (...args) => { calls++; return fetchImpl(...args); } });
      const { error } = await observe(api('/api/command', {}));
      assert.equal(error.category, kind); assert.equal(error.definitiveRejected, false);
      await time.tick(200000); assert.equal(calls, 1); assert.equal(time.timers.size, 0);
    });
  }
});

test('poll_never_overlaps_and_backoff_is_bounded_then_resets', async t => {
  const time = clock(t); let calls = 0, active = 0, maximum = 0, pending;
  const values = [], errors = [];
  const poll = session.createPoll({ read: async () => {
    calls++; active++; maximum = Math.max(maximum, active); pending = deferred();
    try { return await pending.promise; } finally { active--; }
  }, success: value => values.push(value), failure: error => errors.push(error) });
  poll.start(); await time.flush(); assert.equal(calls, 1);
  poll.wake(); poll.wake(); await time.tick(60000); assert.equal(calls, 1);
  pending.resolve('first'); await time.flush(); await time.tick(0); assert.equal(calls, 2);
  for (const delay of [2400, 4800, 9600, 15000, 15000]) {
    const before = calls; pending.reject(Error('offline')); await time.flush();
    await time.tick(delay - 1); assert.equal(calls, before);
    await time.tick(1); assert.equal(calls, before + 1);
  }
  pending.resolve('restored'); await time.flush();
  const before = calls; await time.tick(1199); assert.equal(calls, before);
  await time.tick(1); assert.equal(calls, before + 1); assert.equal(maximum, 1);
  assert.deepEqual(values, ['first', 'restored']); assert.equal(errors.length, 5);
  poll.stop(); pending.resolve('late'); await time.flush(); await time.tick(100000);
  assert.deepEqual(values, ['first', 'restored']); assert.equal(time.timers.size, 0);
});

test('poll_auth_403_stops_until_explicit_restart_and_wake_cannot_bypass_it', async t => {
  const time = clock(t); let calls = 0, expired = true; const errors = [];
  const poll = session.createPoll({ read: async () => { calls++; if (expired) throw Object.assign(Error('expired'), { category: 'http', status: 403 }); return 'ok'; },
    success: () => {}, failure: error => errors.push(error) });
  poll.start(); await time.flush(); assert.equal(calls, 1); assert.equal(errors.length, 1);
  poll.wake(); await time.tick(120000); assert.equal(calls, 1); assert.equal(time.timers.size, 0);
  expired = false; poll.start(); await time.flush(); assert.equal(calls, 2); poll.stop(); assert.equal(time.timers.size, 0);
});

test('independent_polls_keep_state_reading_while_setup_is_pending', async t => {
  const time = clock(t), setupWait = deferred(); let states = 0, setups = 0;
  const state = session.createPoll({ read: async () => ++states, success: () => {}, failure: () => {} });
  const setup = session.createPoll({ read: () => { setups++; return setupWait.promise; }, success: () => {}, failure: () => {} });
  state.start(); setup.start(); await time.flush(); await time.tick(3600);
  assert.equal(states, 4); assert.equal(setups, 1);
  state.stop(); setup.stop(); setupWait.resolve({}); await time.flush(); assert.equal(time.timers.size, 0);
});

test('poll_stop_and_restart_ignores_old_result_without_overlapping_reads', async t => {
  const time = clock(t), waiting = deferred(), values = []; let calls = 0, oldSignal;
  const poll = session.createPoll({ read: ({ signal }) => { calls++; if (calls === 1) { oldSignal = signal; return waiting.promise; } return Promise.resolve('current'); },
    success: value => values.push(value), failure: () => {} });
  poll.start(); await time.flush(); poll.stop(); poll.start(); await time.tick(1000);
  assert.equal(oldSignal.aborted, true); assert.equal(calls, 1);
  waiting.resolve('stale'); await time.flush(); await time.tick(0);
  assert.equal(calls, 2); assert.deepEqual(values, ['current']); poll.stop(); assert.equal(time.timers.size, 0);
});

test('storage_getter_and_get_failures_use_memory_without_crashing', () => {
  const getter = session.safeStorage(() => { throw Error('SecurityError'); });
  assert.equal(getter.available, false); assert.equal(getter.get('relay-session'), null);
  assert.equal(getter.set('relay-session', 'memory-token'), false); assert.equal(getter.get('relay-session'), 'memory-token');
  assert.equal(getter.remove('relay-session'), false); assert.equal(getter.get('relay-session'), null);
  let fail = false;
  const store = session.safeStorage(() => ({ getItem: () => { if (fail) throw Error('blocked'); return 'old'; }, setItem: () => {}, removeItem: () => {} }));
  assert.equal(store.get('relay-project'), 'old'); fail = true;
  assert.equal(store.get('relay-project'), 'old'); assert.equal(store.available, false);
  store.set('relay-project', 'new'); assert.equal(store.get('relay-project'), 'new');
});

test('storage_quota_set_and_remove_failures_never_restore_stale_persistent_values', () => {
  const values = new Map([['pending', 'old']]); let blocked = false;
  const store = session.safeStorage(() => ({ getItem: key => values.get(key) ?? null,
    setItem: (key, value) => { if (blocked) throw Error('QuotaExceededError'); values.set(key, value); },
    removeItem: key => { if (blocked) throw Error('SecurityError'); values.delete(key); } }));
  assert.equal(store.get('pending'), 'old'); blocked = true;
  assert.equal(store.set('pending', 'new'), false); assert.equal(store.get('pending'), 'new'); assert.equal(store.available, false);
  store.remove('pending'); assert.equal(store.get('pending'), null); assert.equal(values.get('pending'), 'old');
  const removing = session.safeStorage(() => ({ getItem: () => 'old', setItem: () => {}, removeItem: () => { throw Error('blocked'); } }));
  removing.get('pending'); assert.equal(removing.remove('pending'), false); assert.equal(removing.get('pending'), null);
});
