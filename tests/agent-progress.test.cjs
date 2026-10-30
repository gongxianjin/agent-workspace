const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const browser = { window: {} };
vm.runInNewContext(fs.readFileSync(path.join(__dirname, '../live-output.js'), 'utf8'), browser);
const progress = (project, telemetry = null) => browser.window.RelayOutput.describeProgress(project, telemetry);
const project = (changes = {}) => ({
  id: 'a'.repeat(32), epoch: 2, revision: 7, phase: 'developing',
  tasks: [{ id: 'D1', title: '页面结构', owner: 'developer' }, { id: 'D2', title: '交互与存储', owner: 'developer' },
    { id: 'T1', title: '独立验收', owner: 'tester' }],
  run: { id: 'c'.repeat(32), done: ['D1', 'D1', 'obsolete'] },
  activeAttempt: { id: 'b'.repeat(32), epoch: 2, projectId: 'a'.repeat(32), runId: 'c'.repeat(32),
    inputHash: 'd'.repeat(64), role: 'developer', kind: 'develop', taskId: 'D2', status: 'running', createdAt: 10 },
  events: [], ...changes
});
const page = (changes = {}) => ({
  scope: { projectId: 'a'.repeat(32), epoch: 2, attemptId: 'b'.repeat(32), runId: 'c'.repeat(32), inputHash: 'd'.repeat(64) },
  authority: { epoch: 2, revision: 7, activeAttemptId: 'b'.repeat(32), phase: 'developing' },
  available: true, sealed: false, serverNow: 30000,
  metadata: { role: 'developer', kind: 'develop', status: 'running', stage: 'cli_running',
    queuedAt: 10000, startedAt: 12000, lastEventAt: 25000 },
  activity: { kind: 'tool', tool: 'write_project', phase: 'finished', outcome: 'ok', at: 25000 }, ...changes
});

test('active_task_and_completed_count_come_from_authoritative_ids_not_approval_or_log', () => {
  const result = progress(project(), page());
  assert.match(result.agent, /开发者/);
  assert.match(result.task, /D2.*交互与存储/);
  assert.match(result.task, /已完成 1.*共 2/);
  assert.match(result.activity, /写入.*已返回/);
  assert.match(result.timing, /18 秒/);
  assert.doesNotMatch(result.title, /通过|完成/);
});

test('queue_host_validation_and_trusted_rerun_are_visible_without_claiming_project_complete', () => {
  const queued = project(); queued.activeAttempt.status = 'queued';
  assert.match(progress(queued, page()).title, /排队|等待执行/);
  assert.doesNotMatch(progress(queued, page()).agent, /CLI 正在运行/);
  for (const [stage, wording] of [['host_validation', /宿主校验/], ['trusted_rerun', /可信.*复跑/], ['cli_exited', /退出.*校验/]]) {
    const telemetry = page(); telemetry.metadata.stage = stage; telemetry.metadata.cliEndedAt = 20000;
    const result = progress(project({ phase: 'testing' }), telemetry);
    assert.match(result.agent, wording);
    assert.match(result.title, /测试/);
    assert.doesNotMatch(result.title, /通过|已完成/);
    assert.match(result.timing, /CLI.*8 秒/);
  }
});

test('foreign_old_or_closed_telemetry_cannot_supply_live_activity_or_stage', () => {
  const cases = [
    value => { value.scope.projectId = 'f'.repeat(32); },
    value => { value.scope.attemptId = 'f'.repeat(32); },
    value => { value.scope.epoch = 1; },
    value => { value.scope.runId = 'f'.repeat(32); },
    value => { value.scope.inputHash = 'f'.repeat(64); },
    value => { value.authority.revision = 6; },
    value => { value.authority.activeAttemptId = null; },
    value => { value.sealed = true; },
    value => { value.metadata.status = 'complete'; },
    value => { value.available = false; }
  ];
  for (const change of cases) {
    const telemetry = page(); change(telemetry); telemetry.metadata.stage = 'trusted_rerun';
    const result = progress(project(), telemetry);
    assert.doesNotMatch(result.agent + result.activity, /可信.*复跑|写入.*已返回/);
    assert.equal(result.title, progress(project()).title);
  }
});

test('missing_or_invalid_timestamps_never_make_up_elapsed_time_and_quiet_output_is_not_failure', () => {
  assert.match(progress(project()).timing, /暂无.*计时|尚无.*计时/);
  for (const timestamp of [null, 0, NaN, -1, 40000]) {
    const telemetry = page(); telemetry.metadata.startedAt = timestamp;
    assert.doesNotMatch(progress(project(), telemetry).timing, /NaN|负|CLI.*\d+ 秒/);
  }
  const telemetry = page({ serverNow: 90000 });
  assert.match(progress(project(), telemetry).activity, /无新.*活动|无新.*事件/);
  assert.doesNotMatch(progress(project(), telemetry).activity, /已失败|正在思考/);
});

test('unknown_lease_blocking_pause_and_human_next_action_remain_authoritative', () => {
  for (const changes of [{ leaseUnknown: true }, { recoveryRequired: true }]) {
    const result = progress(project(changes), page());
    assert.match(result.title, /待核实|未知/);
    assert.match(result.next, /核实|保留/);
    assert.doesNotMatch(result.agent, /CLI 正在运行/);
  }
  assert.match(progress(project({ pauseRequested: true }), page()).detail, /阶段结束.*暂停|阶段结束.*不再/);
  const blocked = progress(project({ phase: 'blocked', activeAttempt: null, lastError: '未收到显式 ACK' }));
  assert.match(blocked.detail, /未收到显式 ACK/);
  assert.match(blocked.next, /阻塞|核实/);
  assert.match(progress(project({ phase: 'ready', activeAttempt: null })).title, /待人工验收/);
  assert.match(progress(project({ phase: 'accepted', activeAttempt: null })).title, /人工验收已记录/);
});

test('newer_recovery_authority_and_inconsistent_attempt_identity_withdraw_live_details', () => {
  for (const change of [value => { value.authority.leaseUnknown = true; }, value => { value.authority.recoveryRequired = true; },
    value => { value.metadata.role = 'tester'; }, value => { value.metadata.kind = 'repair'; }]) {
    const telemetry = page(); change(telemetry);
    assert.doesNotMatch(progress(project(), telemetry).agent, /CLI 正在运行/);
  }
  const wrongProject = project(); wrongProject.activeAttempt.projectId = 'f'.repeat(32);
  assert.doesNotMatch(progress(wrongProject, page()).agent, /CLI 正在运行/);
  const wrongRun = project(); wrongRun.run.id = 'f'.repeat(32);
  assert.doesNotMatch(progress(wrongRun, page()).agent, /CLI 正在运行/);
});

test('invalid_cli_end_and_prestart_activity_do_not_invent_duration_or_quiet_failure', () => {
  for (const end of [0, -1, NaN, 11000, 40000]) {
    const telemetry = page(); telemetry.metadata.cliEndedAt = end;
    assert.doesNotMatch(progress(project(), telemetry).timing, /CLI.*\d+ 秒/);
  }
  const telemetry = page(); telemetry.metadata.lastEventAt = 1;
  telemetry.serverNow = 90000;
  assert.doesNotMatch(progress(project(), telemetry).activity, /无新/);
});

test('no_active_agent_does_not_make_all_three_roles_look_running_or_invent_a_task', () => {
  const result = progress(project({ phase: 'draft', tasks: [], run: null, activeAttempt: null }));
  assert.match(result.agent, /没有.*运行|暂无.*Agent|尚无.*Agent/);
  assert.match(result.task, /尚未拆分|暂无.*任务/);
  assert.match(result.next, /需求/);
  assert.doesNotMatch(result.task + result.timing, /\d+%|预计.*秒|已完成 0.*共 0/);
});
