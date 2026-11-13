use std::{collections::BTreeMap, path::PathBuf};
use agentranch::core::service::{AppSnapshot, ProjectSnapshot, UiCommand};
use eframe::egui::{self, RichText};
use super::{action::action, pending::Pending};

/// Editable input and selection only. All document/review/execution facts come from core.
pub struct Projects {
    pub selected: Option<String>,
    name: String,
    path: String,
    config_path: String,
    review_reasons: BTreeMap<String, String>,
    repair_reason: String,
    #[cfg(windows)]
    picker: Option<std::sync::mpsc::Receiver<(String, Option<PathBuf>)>>,
}
impl Default for Projects {
    fn default() -> Self {
        Self { selected: None, name: "新项目".into(), path: String::new(), config_path: String::new(), review_reasons: BTreeMap::new(), repair_reason: String::new(), #[cfg(windows)] picker: None }
    }
}
impl Projects {
    pub fn show(&mut self, ui: &mut egui::Ui, snapshot: &AppSnapshot, pending: &Pending, receipts: &[(String, String)]) -> Vec<(String, UiCommand)> {
        let mut commands = Vec::new();
        #[cfg(windows)]
        if let Some(receiver) = &self.picker {
            match receiver.try_recv() {
                Ok((project_id, path)) => {
                    if self.selected.as_deref() == Some(project_id.as_str()) { if let Some(path) = path { self.path = path.display().to_string(); } }
                    self.picker = None;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => self.picker = None,
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        }
        if self.selected.as_ref().is_none_or(|id| !snapshot.projects.iter().any(|p| &p.id == id)) {
            self.selected = snapshot.projects.first().map(|p| p.id.clone());
        }
        egui::Panel::left("project-list").resizable(true).default_size(255.).min_size(200.).show(ui, |ui| {
            ui.heading("项目");
            ui.label("项目名称");
            ui.text_edit_singleline(&mut self.name);
            action(ui, pending, &mut commands, "新建项目", !self.name.trim().is_empty(), "project-create", "project-create".into(), UiCommand::ProjectCreate { name: self.name.trim().into() });
            ui.separator();
            egui::ScrollArea::vertical().id_salt("project-list-scroll").show(ui, |ui| {
                for project in &snapshot.projects {
                    if ui.selectable_label(self.selected.as_deref() == Some(&project.id), format!("{}\n{}", project.name, project.phase)).clicked() {
                        self.selected = Some(project.id.clone());
                        self.path.clear();
                        self.repair_reason.clear();
                    }
                }
                if snapshot.projects.is_empty() { ui.label("先新建项目，再导入已定稿设计。"); }
            });
        });
        egui::CentralPanel::default().show(ui, |ui| {
            egui::ScrollArea::vertical().id_salt("project-detail-scroll").show(ui, |ui| {
                ui.heading("从定稿设计文档开始");
                ui.label("导入已完成的设计，不重新生成设计。确认上传版本后拆分、逐项审核，再由你手动开始开发。");
                ui.small("导入文件、加载配置不会自动调用模型。未配置与本版未实现是不同阻塞；配置不代表执行链已可用，以核心给出的原因和实际执行记录为准。");
                egui::CollapsingHeader::new("执行依赖配置").id_salt("project-config").show(ui, |ui| {
                    ui.label("配置文件路径（由后台读取，不自动启动任务）");
                    ui.text_edit_singleline(&mut self.config_path);
                    action(ui, pending, &mut commands, "加载执行配置", !self.config_path.trim().is_empty(), "project-configure", "project-configure".into(), UiCommand::ProjectConfigure { config_path: PathBuf::from(self.config_path.trim()) });
                });
                if let Some(project) = snapshot.projects.iter().find(|p| self.selected.as_deref() == Some(&p.id)) {
                    self.detail(ui, project, pending, &mut commands);
                } else { ui.label("请在左侧新建或选择项目。"); }
                for error in &snapshot.errors {
                    ui.separator();
                    ui.strong(format!("{}：{}", error.code, error.message));
                    ui.label(format!("下一步：{}", error.next_step));
                }
                ui.separator();
                egui::CollapsingHeader::new("操作回执").id_salt("project-receipts").show(ui, |ui| {
                    ui.small("核心接受请求不等于外部执行完成；以项目阶段、公开输出和独立测试结果为准。");
                    for (title, detail) in receipts {
                        egui::CollapsingHeader::new(title).show(ui, |ui| { ui.add(egui::Label::new(RichText::new(detail).monospace()).wrap()); });
                    }
                });
            });
        });
        commands
    }
    fn detail(&mut self, ui: &mut egui::Ui, project: &ProjectSnapshot, pending: &Pending, commands: &mut Vec<(String, UiCommand)>) {
        let id = &project.id;
        ui.separator();
        ui.heading(&project.name);
        ui.label(format!("当前阶段：{}", project.phase));
        egui::CollapsingHeader::new("项目与产物目录").id_salt(("project-directories", id)).show(ui, |ui| {
            ui.add(egui::Label::new(format!("项目目录：{}", if project.project_path.is_empty() { "尚未准备目录" } else { &project.project_path })).wrap());
            ui.add(egui::Label::new(format!("产物目录：{}", if project.product_path.is_empty() { "尚未准备目录" } else { &project.product_path })).wrap());
            ui.small("目录存在不代表已生成产物或开发完成。");
            ui.small("刷新项目会由后台准备或校验受管目录；旧项目首次刷新可能创建目录，不启动开发。");
            action(ui, pending, commands, "刷新项目", true, &format!("project:{id}:refresh"), format!("refresh:{id}"), UiCommand::ProjectRefresh { project_id: id.clone() });
        });
        for reason in &project.blockers { ui.strong(format!("阻塞：{reason}")); }
        ui.separator();
        ui.strong("定稿设计文档");
        ui.label("UTF-8 .md / .txt，原文件不超过 256 KiB。输入文件路径，或把文件拖到此窗口。");
        let dropped = ui.ctx().input(|i| i.raw.dropped_files.first().map(|f| f.path().to_path_buf()));
        if let Some(path) = dropped { self.path = path.display().to_string(); }
        ui.text_edit_singleline(&mut self.path);
        #[cfg(windows)]
        if ui.add_enabled(self.picker.is_none(), egui::Button::new("选择设计文件…")).clicked() {
            let (sender, receiver) = std::sync::mpsc::sync_channel(1);
            self.picker = Some(receiver);
            let project_id = id.clone();
            let ctx = ui.ctx().clone();
            std::thread::spawn(move || { let path = pick_document(); let _ = sender.send((project_id, path)); ctx.request_repaint(); });
        }
        action(ui, pending, commands, "导入设计文档", !self.path.trim().is_empty(), &format!("project:{id}:import"), format!("import:{id}"), UiCommand::ProjectImport { project_id: id.clone(), path: PathBuf::from(self.path.trim()) });
        if let Some(document) = &project.document {
            ui.label(format!("上传版本 U{} · {} · {}", document.version, document.filename, if document.confirmed { "已确认" } else { "待确认" }));
            ui.label(format!("SHA-256：{}", document.sha256));
            egui::ScrollArea::vertical().id_salt(("document-preview", id, document.version)).max_height(200.).show(ui, |ui| {
                ui.add(egui::Label::new(&document.text).wrap());
            });
            action(ui, pending, commands, "确认此上传版本", project.can_confirm, &format!("project:{id}:confirm:{}", document.version), format!("confirm:{id}:{}", document.version), UiCommand::ProjectConfirm { project_id: id.clone(), version: document.version, sha256: document.sha256.clone() });
        } else { ui.label("尚未导入文档，不能拆分或开始开发。"); }
        action(ui, pending, commands, "按已确认上传版拆分任务", project.can_split, &format!("project:{id}:split"), format!("split:{id}"), UiCommand::ProjectSplit { project_id: id.clone() });
        ui.separator();
        ui.strong("逐项审核任务");
        if project.tasks.is_empty() { ui.label("暂无拆分任务。未执行拆分，或执行依赖尚未就绪；不会用固定样例代替。"); }
        for task in &project.tasks {
            ui.group(|ui| {
                ui.strong(format!("{} · {}", task.title, task.review));
                ui.label(&task.description);
                ui.label(format!("验收标准：{}", task.acceptance));
                if let Some(reason) = &task.reason { ui.label(format!("已提交原因：{reason}")); }
                let reason = self.review_reasons.entry(format!("{id}:{}", task.id)).or_default();
                ui.label("驳回原因");
                ui.text_edit_multiline(reason);
                ui.horizontal_wrapped(|ui| {
                    action(ui, pending, commands, "审核通过", task.review != "approved", &format!("project:{id}:task:{}:approve", task.id), format!("review:{id}:{}", task.id), UiCommand::TaskReview { project_id: id.clone(), task_id: task.id.clone(), approve: true, reason: String::new() });
                    action(ui, pending, commands, "驳回此项", !reason.trim().is_empty(), &format!("project:{id}:task:{}:reject", task.id), format!("review:{id}:{}", task.id), UiCommand::TaskReview { project_id: id.clone(), task_id: task.id.clone(), approve: false, reason: reason.trim().into() });
                    action(ui, pending, commands, "仅重做此项", task.review == "rejected", &format!("project:{id}:task:{}:redo", task.id), format!("redo:{id}:{}", task.id), UiCommand::TaskRedo { project_id: id.clone(), task_id: task.id.clone() });
                });
            });
        }
        ui.label("全部任务审核通过后，只解锁手动开始；不会自动进入开发。");
        action(ui, pending, commands, "手动开始开发", project.can_start, &format!("project:{id}:start"), format!("start-project:{id}"), UiCommand::ProjectStart { project_id: id.clone() });
        ui.separator();
        ui.strong("开发进度与公开输出");
        if project.actions.is_empty() { ui.label("还没有执行记录。未执行不等于完成。"); }
        for a in &project.actions {
            ui.group(|ui| {
                ui.strong(format!("{} · {}", a.kind, a.state));
                if let Some(session) = &a.session { ui.small(format!("会话：{session}")); }
                ui.add(egui::Label::new(&a.detail).wrap());
            });
        }
        ui.separator();
        ui.strong("独立测试、返修与人工验收");
        action(ui, pending, commands, "运行独立测试", project.can_test, &format!("project:{id}:test"), format!("test:{id}"), UiCommand::ProjectTest { project_id: id.clone() });
        ui.label("返修原因（测试未通过时必须显式发起返修，之后再复测）");
        ui.text_edit_multiline(&mut self.repair_reason);
        action(ui, pending, commands, "提交返修", project.can_repair && !self.repair_reason.trim().is_empty(), &format!("project:{id}:repair"), format!("repair:{id}"), UiCommand::ProjectRepair { project_id: id.clone(), reason: self.repair_reason.trim().into() });
        action(ui, pending, commands, "人工验收通过", project.can_accept, &format!("project:{id}:accept"), format!("accept:{id}"), UiCommand::ProjectAccept { project_id: id.clone() });
    }
}
#[cfg(windows)]
fn pick_document() -> Option<PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::UI::Controls::Dialogs::{GetOpenFileNameW, OPENFILENAMEW, OFN_FILEMUSTEXIST, OFN_PATHMUSTEXIST, OFN_NOCHANGEDIR, OFN_DONTADDTORECENT};
    let mut file = vec![0u16; 32768];
    let filter: Vec<u16> = "定稿设计文档 (*.md;*.txt)\0*.md;*.txt\0\0".encode_utf16().collect();
    let title: Vec<u16> = "选择已定稿设计文档（选择后仍需点击导入）\0".encode_utf16().collect();
    let mut dialog = OPENFILENAMEW {
        lStructSize: std::mem::size_of::<OPENFILENAMEW>() as u32,
        lpstrFilter: filter.as_ptr(),
        lpstrFile: file.as_mut_ptr(),
        nMaxFile: file.len() as u32,
        lpstrTitle: title.as_ptr(),
        Flags: OFN_FILEMUSTEXIST | OFN_PATHMUSTEXIST | OFN_NOCHANGEDIR | OFN_DONTADDTORECENT,
        ..Default::default()
    };
    // Buffers outlive the synchronous native call, made only on this background thread.
    if unsafe { GetOpenFileNameW(&mut dialog) } == 0 { return None; }
    let length = file.iter().position(|v| *v == 0)?;
    Some(std::ffi::OsString::from_wide(&file[..length]).into())
}
