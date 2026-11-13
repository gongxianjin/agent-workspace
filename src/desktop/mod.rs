pub mod action;
pub mod approval;
pub mod launch;
pub mod pending;
pub mod projects;
use std::{collections::VecDeque, sync::Arc, time::{Duration, SystemTime, UNIX_EPOCH}};
use agentranch::{config::Mode, core::service::{AppSnapshot, ServiceHandle, ServiceOptions, UiCommand, SubmitError, Readiness, RunView}};
use eframe::egui::{self, RichText};

struct Receipt { id: u64, action: String, detail: String, failed: bool }
/// UI selection, receipts and pending interactions only. Domain state is always core's Arc snapshot.
pub struct Workbench {
    service: ServiceHandle,
    pending: pending::Pending,
    receipts: VecDeque<Receipt>,
    selected: Option<String>,
    template: String,
    gateway_config: String,
    gateway_worker: String,
    gateway_project_doc: String,
    state_dir: String,
    notice: Option<String>,
    confirm_close: bool,
    closing: bool,
    close_requested: bool,
    projects: projects::Projects,
    diagnostics: bool,
}
impl Workbench {
    pub fn new(options: ServiceOptions, ctx: &egui::Context) -> anyhow::Result<Self> {
        let mut visuals = egui::Visuals::light();
        visuals.override_text_color = Some(egui::Color32::from_rgb(34, 50, 71));
        visuals.panel_fill = egui::Color32::from_rgb(245, 247, 250);
        ctx.set_theme(egui::ThemePreference::Light);
        ctx.set_visuals_of(egui::Theme::Light, visuals);
        let mut style = (*ctx.style_of(egui::Theme::Light)).clone();
        style.text_styles.insert(egui::TextStyle::Body, egui::FontId::proportional(16.));
        style.text_styles.insert(egui::TextStyle::Button, egui::FontId::proportional(16.));
        style.text_styles.insert(egui::TextStyle::Heading, egui::FontId::proportional(22.));
        style.spacing.item_spacing = egui::vec2(10., 8.);
        ctx.set_style_of(egui::Theme::Light, style);
        let repaint = ctx.clone();
        let state_dir = options.state_dir.display().to_string();
        let template = if options.mode == Mode::Local { "sum-native-v1" } else { "sum-v1" }.into();
        let service = ServiceHandle::spawn(options, Arc::new(move || repaint.request_repaint()))?;
        Ok(Self { service, pending: Default::default(), receipts: VecDeque::new(), selected: None, template, gateway_config: String::new(), gateway_worker: String::new(), gateway_project_doc: String::new(), state_dir, notice: None, confirm_close: false, closing: false, close_requested: false, projects: Default::default(), diagnostics: false })
    }
    pub fn poll(&mut self) {
        for result in self.service.drain_results() {
            let action = self.pending.complete(result.request_id).unwrap_or_else(|| "未关联请求".into());
            let (detail, failed) = match result.result {
                Ok(value) => {
                    if let Some(id) = value.get("id").and_then(|v| v.as_str()) { self.selected = Some(id.into()); self.projects.selected = Some(id.into()); }
                    (serde_json::to_string_pretty(&value).unwrap_or_default(), false)
                }
                Err(e) => (format!("{}: {}\n下一步：{}", e.code, e.message, e.next_step), true),
            };
            self.receipts.push_front(Receipt { id: result.request_id, action, detail, failed });
            self.receipts.truncate(20);
        }
    }
    pub fn snapshot(&mut self) -> Arc<AppSnapshot> { self.service.snapshot() }
    pub fn send(&mut self, key: String, command: UiCommand) {
        if self.closing || self.pending.contains(&key) { return; }
        match self.service.try_submit(command) {
            Ok(id) => { self.pending.accept(key, id); self.notice = None; }
            Err(SubmitError::Busy) => self.notice = Some("后台忙：此操作尚未接受，请稍后再试。".into()),
            Err(SubmitError::Stopped) => self.notice = Some("后台已停止：此操作未执行。重新打开工作台。".into()),
        }
    }
    pub fn pending_count(&self) -> usize { self.pending.len() }
    #[cfg(test)]
    pub fn receipt_count(&self) -> usize { self.receipts.len() }
    fn button(&mut self, ui: &mut egui::Ui, label: &str, enabled: bool, key: String, command: UiCommand) {
        let pending = self.pending.contains(&key);
        let label = if pending { format!("{label}（等待回执）") } else { label.into() };
        if ui.add_enabled(enabled && !pending, egui::Button::new(label)).clicked() { self.send(key, command); }
    }
    pub fn show(&mut self, ui: &mut egui::Ui) {
        self.poll();
        let snapshot = self.snapshot();
        let ctx = ui.ctx().clone();
        ctx.request_repaint_after(Duration::from_millis(250));
        if ctx.input(|i| i.viewport().close_requested()) && !self.close_requested {
            // eframe exits at the end of this frame unless closure is cancelled.
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            if !self.closing {
                if snapshot.unfinished > 0 || self.pending_count() > 0 { self.confirm_close = true; }
                else { self.begin_close(); }
            }
        }
        if self.closing {
            if !snapshot.running && self.pending_count() == 0 {
                if !self.close_requested {
                    self.close_requested = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            } else {
                ui.heading("正在关闭工作台");
                ui.label(format!("等待后台停止并收齐操作回执（剩余 {} 项）。关闭不代表远端任务已取消。", self.pending_count()));
                ctx.request_repaint_after(Duration::from_millis(20));
            }
            return;
        }
        egui::Panel::top("mode-toolbar").show(ui, |ui| {
            ui.heading("agentranch 工作台");
            ui.strong(if !self.diagnostics { "设计文档工作流 · 真实执行须配置 AO / Reason" } else if snapshot.mode == Mode::Local { "本地体验 · 无真实 AO/Reason/模型" } else { "真实模式 · 直接 SSH Worker；配置不等于远端已验证" });
            ui.horizontal_wrapped(|ui| {
                ui.label(format!("服务：{}", if self.diagnostics { readiness(&snapshot.readiness) } else if snapshot.running { "后台运行；执行依赖见项目阻塞" } else { "后台已停止" }));
                ui.label(format!("等待回执：{}", self.pending_count()));
                ui.selectable_value(&mut self.diagnostics, false, "设计文档工作流");
                ui.selectable_value(&mut self.diagnostics, true, "本地诊断（可选）");
                if self.diagnostics && snapshot.mode == Mode::Local {
                    self.button(ui, "启动本地体验", matches!(snapshot.readiness, Readiness::NotStarted | Readiness::NotReady), "start".into(), UiCommand::StartLocal);
                }
                if ui.button("关闭工作台").clicked() {
                    if snapshot.unfinished > 0 || self.pending_count() > 0 { self.confirm_close = true; }
                    else { self.begin_close(); }
                }
            });
            if !self.diagnostics && snapshot.mode == Mode::Local { ui.small("本地体验仅用于可选诊断；项目真实执行不会回退到模拟 AO / Reason / 模型。"); }
            ui.small(format!("状态目录：{}", self.state_dir));
            if let Some(notice) = &self.notice { ui.strong(notice); }
        });
        egui::Panel::bottom("dependencies").resizable(true).default_size(155.).show(ui, |ui| {
            egui::ScrollArea::vertical().id_salt("dependency-scroll").max_height(220.).show(ui, |ui| {
                ui.strong("节点与依赖");
                for n in &snapshot.nodes {
                    ui.label(format!("{}  名额 {}/{}  准入={} 健康={} 排空={}  {}", n.id, n.used, n.slots, n.admitted, n.healthy, n.draining, n.address));
                    ui.small(format!("来源：{}；观测：{}", n.source, stamp(n.observed_at)));
                }
                for d in &snapshot.dependencies {
                    ui.label(format!("{}：{} · {}", d.id, d.state, d.detail));
                    ui.small(format!("来源：{}；观测：{}", d.source, stamp(d.observed_at)));
                }
                ui.small(format!("快照生成：{}；不是远端进度证明。关闭窗口不等于取消任务。", stamp(snapshot.observed_at)));
            });
        });
        if !self.diagnostics {
            if snapshot.mode == Mode::Real {
                egui::Panel::top("direct-ssh-manager").default_size(190.).show(ui, |ui| {
                    ui.strong("直接 SSH Worker");
                    ui.small("只使用显式 known_hosts、普通用户 key、固定 Worker artifact；未完成终态保留名额。");
                    ui.horizontal(|ui| {
                        ui.label("Gateway 配置"); ui.text_edit_singleline(&mut self.gateway_config);
                        if ui.button("加载 SSH 配置").clicked() && !self.gateway_config.trim().is_empty() {
                            self.send("gateway-configure".into(), UiCommand::GatewayConfigure { config_path: self.gateway_config.trim().into() });
                        }
                    });
                    if snapshot.gateway.is_none() {
                        ui.horizontal(|ui| {
                            ui.add_enabled(false, egui::Button::new("执行 sum-v1"));
                            ui.add_enabled(false, egui::Button::new("执行 project-work（模型生成页面）"));
                        });
                    }
                    if let Some(gateway)=&snapshot.gateway {
                        ui.label(format!("Worker {} @ {} · artifact {} · 已占用 {} · 远端已验证={} ",gateway.worker_id,gateway.host,&gateway.artifact_sha256[..8],gateway.used_slots,gateway.remote_verified));
                        ui.horizontal(|ui| {
                            egui::ComboBox::from_id_salt("ssh-worker").selected_text(if self.gateway_worker.is_empty(){"选择 Worker"}else{&self.gateway_worker}).show_ui(ui,|ui|{
                                ui.selectable_value(&mut self.gateway_worker,gateway.worker_id.clone(),&gateway.worker_id);
                            });
                            self.button(ui,"执行 sum-v1",gateway.used_slots==0&&self.gateway_worker==gateway.worker_id,"gateway-dispatch".into(),UiCommand::GatewayDispatch{worker_id:gateway.worker_id.clone()});
                            if ui.button("查看同一执行证据").clicked(){self.diagnostics=true;}
                        });
                        ui.horizontal(|ui| {
                            ui.label("设计文档"); ui.text_edit_singleline(&mut self.gateway_project_doc);
                            let ready=gateway.used_slots==0&&self.gateway_worker==gateway.worker_id&&!self.gateway_project_doc.trim().is_empty();
                            if ui.add_enabled(ready,egui::Button::new("执行 project-work（模型生成页面）")).clicked() {
                                self.send("gateway-dispatch-project".into(),UiCommand::GatewayDispatchProject{worker_id:gateway.worker_id.clone(),doc_path:self.gateway_project_doc.trim().into()});
                            }
                        });
                        ui.small("project-work 需要带模型 token 的帧式 --gateway-key-stdin 启动；文档为 UTF-8 设计 markdown，产物为静态页面文件。");
                        for view in &snapshot.runs {
                            ui.monospace(format!("{} · attempt {} · session {:?} · {}",view.run.id,view.run.attempt,view.run.session,view.run.status));
                        }
                    }
                });
            }
            let receipts: Vec<_> = self.receipts.iter().map(|r| (format!("#{} {} · {}", r.id, r.action, if r.failed { "失败" } else { "核心已返回" }), r.detail.clone())).collect();
            for (key, command) in self.projects.show(ui, &snapshot, &self.pending, &receipts) { self.send(key, command); }
        } else {
        egui::Panel::left("tasks").resizable(true).default_size(335.).min_size(235.).show(ui, |ui| {
            ui.strong("任务");
            let templates = if snapshot.mode == Mode::Local { ["sum-native-v1", "control-native-v1"] } else { ["sum-v1", "control-v1"] };
            egui::ComboBox::from_id_salt("template").selected_text(&self.template).show_ui(ui, |ui| {
                for template in templates { ui.selectable_value(&mut self.template, template.into(), template); }
            });
            ui.small(if self.template.starts_with("sum") { "计算 1 + 2 + 3，并独立核验产物。" } else { "请求一次审批，然后可取消受管进程。" });
            self.button(ui, "创建任务", snapshot.readiness == Readiness::Ready, format!("dispatch:{}", self.template), UiCommand::Dispatch { template: self.template.clone() });
            ui.separator();
            egui::ScrollArea::vertical().id_salt("run-scroll").show(ui, |ui| {
                if snapshot.runs.is_empty() { ui.label("还没有任务。启动本地体验后创建一个任务。"); }
                for view in &snapshot.runs {
                    let run = &view.run;
                    if ui.selectable_label(self.selected.as_deref() == Some(run.id.as_str()), format!("{}\n{}  第 {} 次\n{}", run.template, run.status, run.attempt, run.id)).clicked() { self.selected = Some(run.id.clone()); }
                    ui.separator();
                }
            });
        });
        egui::CentralPanel::default().show(ui, |ui| {
            egui::ScrollArea::vertical().id_salt("detail-scroll").show(ui, |ui| {
                if let Some(view) = snapshot.runs.iter().find(|r| self.selected.as_deref() == Some(r.run.id.as_str())) {
                    self.run_detail(ui, view);
                    for approval in snapshot.approvals.iter().filter(|p| p.run_id == view.run.id) {
                        ui.separator();
                        ui.strong(if approval.simulated { "待审批（模拟）" } else { "待审批" });
                        ui.label(&approval.summary);
                        ui.small(format!("请求 {}；第 {} 次；generation {}；过期 {}", approval.request_id, approval.attempt, approval.generation, stamp(approval.expires_at)));
                        ui.horizontal_wrapped(|ui| {
                            for offered in &approval.offered {
                                let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
                                let command = approval::decision_command(approval, &offered.id, now);
                                let key = format!("decide:{}:{}:{}", approval.run_id, approval.request_id, approval.generation);
                                if ui.add_enabled(view.can_decide && command.is_some() && !self.pending.contains(&key), egui::Button::new(&offered.label)).clicked() {
                                    if let Some(command) = command { self.send(key, command); }
                                }
                            }
                        });
                    }
                } else {
                    ui.heading("选择任务查看执行证据");
                    ui.label("任务状态、审批、结果来自真实本地核心和 SQLite。AO、Reason 与模型在本地体验中是模拟依赖。");
                }
                for e in &snapshot.errors { ui.separator(); ui.strong(format!("{}：{}", e.code, e.message)); ui.label(format!("下一步：{}", e.next_step)); }
                ui.separator();
                ui.strong("操作回执（最近 20 条）");
                ui.small("接受请求或 state=queued 仅表示已记录意图，不表示远端执行或审批已完成。");
                for receipt in &self.receipts {
                    egui::CollapsingHeader::new(format!("#{} {} · {}", receipt.id, receipt.action, if receipt.failed { "失败" } else { "核心已返回" })).id_salt(("receipt", receipt.id)).show(ui, |ui| {
                        ui.add(egui::Label::new(RichText::new(&receipt.detail).monospace()).wrap());
                    });
                }
            });
        });
        }
        if self.confirm_close {
            egui::Modal::new(egui::Id::unique("confirm-close")).show(&ctx, |ui| {
                ui.heading("仍有未完成工作");
                ui.label(format!("{} 个未完成任务，{} 个请求等待回执。", snapshot.unfinished, self.pending_count()));
                ui.label("关闭只停止本地工作台，不证明远端已取消。未确认的任务可能继续，重开后可能显示 UNKNOWN。");
                ui.horizontal(|ui| {
                    if ui.button("继续查看").clicked() { self.confirm_close = false; }
                    if ui.button("仍然关闭").clicked() { self.begin_close(); }
                });
            });
        }
    }
    fn begin_close(&mut self) {
        self.confirm_close = false;
        self.closing = true;
        self.service.stop();
    }
    fn run_detail(&mut self, ui: &mut egui::Ui, view: &RunView) {
        let r = &view.run;
        ui.heading(&r.template);
        ui.label(format!("状态：{}；第 {} 次；{}", r.status, r.attempt, if view.read_only { "只读" } else { "本地入口所属" }));
        ui.monospace(&r.id);
        if let Some(reason) = &view.disabled_reason { ui.label(reason); }
        ui.horizontal_wrapped(|ui| {
            self.button(ui, "查看状态", view.can_status, format!("status:{}", r.id), UiCommand::Status { run_id: r.id.clone() });
            self.button(ui, "刷新审批", view.can_approvals, format!("approvals:{}", r.id), UiCommand::Approvals { run_id: r.id.clone() });
            self.button(ui, "查看报告", view.can_report, format!("report:{}", r.id), UiCommand::Report { run_id: r.id.clone() });
            self.button(ui, "重试", view.can_retry, format!("retry:{}", r.id), UiCommand::Retry { run_id: r.id.clone(), attempt: r.attempt });
            self.button(ui, "取消任务", view.can_cancel, format!("cancel:{}", r.id), UiCommand::Cancel { run_id: r.id.clone() });
        });
        for (label, field) in [("创建", "created_at"), ("实际进度", "progress_at"), ("实际联系", "contact_at")] {
            ui.small(format!("{label}：{}", r.detail.get(field).and_then(|v| v.as_u64()).map(stamp).unwrap_or_else(|| "未观测".into())));
        }
        for field in ["contact_source", "progress_source", "reason", "failure", "artifact_dir"] {
            if let Some(value) = r.detail.get(field) { ui.label(format!("{field}：{value}")); }
        }
        ui.monospace(format!("epoch={} session={:?}",r.epoch,r.session));
        for (label, field) in [("进程 Launch 证据", "owner_launch"), ("结果报告", "report"), ("独立核验", "verification"), ("退出证据", "exit_proof")] {
            if let Some(value) = r.detail.get(field) {
                ui.separator(); ui.strong(label);
                ui.add(egui::Label::new(RichText::new(serde_json::to_string_pretty(value).unwrap_or_default()).monospace()).wrap());
            }
        }
    }
}
fn readiness(value: &Readiness) -> &'static str { match value { Readiness::NotStarted => "未启动", Readiness::Starting => "启动中", Readiness::Ready => "已就绪", Readiness::NotReady => "未就绪", Readiness::Stopped => "已停止" } }
fn stamp(value: u64) -> String { if value == 0 { "未观测".into() } else { format!("{value}（Unix 秒）") } }
