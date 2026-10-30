'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const path = require('node:path');
const modulePath = path.join(__dirname, '../live-agents.js');
let agents;
try { agents = require(modulePath); } catch (error) { if (error.code !== 'MODULE_NOT_FOUND') throw error; agents = {}; }
const project = { id: 'a'.repeat(32), revision: 7, epoch: 1 };
function fixture(api) {
  const values = new Map(), changed = [], states = [];
  const storage = { get: key => values.get(key) ?? null, set: (key, value) => values.set(key, value), remove: key => values.delete(key) };
  assert.equal(typeof agents.createCommands, 'function', 'Agent controls need persistent payload-free unknown-command recovery');
  const commands = agents.createCommands({ api, storage, onState: state => states.push(state), onChange: () => changed.push(true), makeId: () => 'fixture-command', now: () => 1234 });
  return { commands, values, storage, states, changed };
}
test('unknown message POST locks the project across role switches, persists no body and never replays', async () => {
  const calls = [];
  const value = fixture(async (route, body) => { calls.push({ route, body }); if (body) throw Object.assign(Error('unknown'), { category: 'timeout' }); return { found: false }; });
  await assert.rejects(value.commands.submit(project, 'developer', 'agent_message', { text: 'PRIVATE-DRAFT', intent: 'clarification', targetAttemptId: null }), /unknown/);
  assert.ok(value.commands.pending(project.id));
  assert.equal(JSON.stringify([...value.values]).includes('PRIVATE-DRAFT'), false);
  assert.deepEqual(Object.keys(value.commands.pending(project.id)).sort(), ['projectId', 'role', 'commandId', 'kind', 'expectedRevision', 'time'].sort());
  await assert.rejects(value.commands.submit(project, 'architect', 'agent_consult', { messageIds: ['PRIVATE-ID'] }), /核实/);
  await value.commands.reconcile(project.id);
  assert.ok(value.commands.pending(project.id), 'Missing receipt is not proof of rejection');
  assert.equal(calls.filter(call => call.body).length, 1);
});
test('found receipt plus current state resolves the pending command, without claiming current phase from receipt', async () => {
  let finishPost, releaseReceipt;
  const current = { ...project, revision: 10, phase: 'paused' };
  const value = fixture((route, body) => body ? new Promise(resolve => { finishPost = resolve; }) : route.startsWith('/api/receipts') ? new Promise(resolve => { releaseReceipt = resolve; }) : Promise.resolve({ projects: [current] }));
  const posted = value.commands.submit(project, 'developer', 'agent_handoff', {});
  const first = value.commands.reconcile(project.id);
  const second = value.commands.reconcile(project.id);
  releaseReceipt({ found: true, acceptedRevision: 8, attemptId: 'old-stage' });
  await Promise.all([first, second]);
  assert.equal(value.commands.pending(project.id), null);
  assert.deepEqual(value.states.at(-1), current);
  finishPost({ ...project, revision: 8, phase: 'developing' });
  await posted;
  assert.equal(value.states.at(-1).revision, 10, 'Late POST cannot roll back confirmed current state');
});
test('reload restores only bounded validated metadata and reconciles read-only', async () => {
  const calls = [];
  const value = fixture(async (route, body) => { calls.push({ route, body }); if (body) throw Error('disconnected'); return { found: false }; });
  await assert.rejects(value.commands.submit(project, 'tester', 'agent_resume', { messageIds: ['not-stored'] }));
  value.commands.stop();
  const restored = agents.createCommands({ api: async (route, body) => { calls.push({ route, body }); return { found: false }; }, storage: value.storage, onState() {}, onChange() {} });
  assert.equal(restored.pending(project.id).kind, 'agent_resume');
  await restored.reconcile(project.id);
  assert.equal(calls.filter(call => call.body).length, 1);
});
test('only definitive rejection unlocks an unknown command without a found receipt', async () => {
  for (const definitiveRejected of [false, true]) {
    const value = fixture(async () => { throw Object.assign(Error('rejected fixture'), { category: 'http', status: 409, definitiveRejected }); });
    await assert.rejects(value.commands.submit(project, 'architect', 'agent_message', { text: 'no public body' }));
    assert.equal(!!value.commands.pending(project.id), !definitiveRejected);
  }
});
test('stopping revokes late receipt/state callbacks but preserves unknown metadata for reload', async () => {
  let finishState;
  const value = fixture((route, body) => body ? Promise.reject(Error('lost')) : route.startsWith('/api/receipts') ? Promise.resolve({ found: true }) : new Promise(resolve => { finishState = resolve; }));
  await assert.rejects(value.commands.submit(project, 'developer', 'agent_handoff', {}));
  const read = value.commands.reconcile(project.id);
  await Promise.resolve();
  value.commands.stop();
  finishState({ projects: [{ ...project, revision: 9 }] }); await read;
  assert.equal(value.states.length, 0);
  assert.ok(value.commands.pending(project.id));
});
test('late POST after session stop cannot clear persisted unknown metadata or publish state', async () => {
  let finishPost;
  const value = fixture(() => new Promise(resolve => { finishPost = resolve; }));
  const posted = value.commands.submit(project, 'developer', 'agent_handoff', {});
  value.commands.stop();
  finishPost({ ...project, revision: 8 }); await posted;
  assert.equal(value.states.length, 0);
  assert.ok(value.commands.pending(project.id), 'An expired session must not resolve pending state from a late POST');
});
test('external newer state prevents stale POST and receipt state from rolling back UI', async () => {
  let finishPost;
  const value = fixture(() => new Promise(resolve => { finishPost = resolve; }));
  const posted = value.commands.submit(project, 'developer', 'agent_handoff', {});
  assert.equal(typeof value.commands.observe, 'function');
  value.commands.observe({ ...project, revision: 20 });
  finishPost({ ...project, revision: 8 }); await posted;
  assert.equal(value.states.length, 0);
});
test('role statuses never turn queued or unverified claimed stages into running CLI evidence', () => {
  assert.equal(typeof agents.describeRoles, 'function');
  const queued = agents.describeRoles({ ...project, activeAttempt: { id: 'b'.repeat(32), role: 'developer', status: 'queued', kind: 'develop', taskId: 'D1' } });
  assert.match(queued.developer.status, /排队/);
  assert.doesNotMatch(queued.developer.status, /执行中|运行中/);
  assert.match(queued.architect.status, /待命|无启动记录/);
  const unknown = agents.describeRoles({ ...project, leaseUnknown: true, activeAttempt: null });
  for (const role of ['architect', 'developer', 'tester']) assert.match(unknown[role].status, /未知/);
});
