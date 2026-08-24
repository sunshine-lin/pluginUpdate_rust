//! 插件心跳与指令下发（DEV-124702 阶段一）
//!
//! # 为什么是「插件轮询」而不是「客户端主动推」
//! 插件运行在浏览器沙箱内，没有可被外部连接的地址——客户端无法主动连它。
//! 故只能由插件定期 POST 心跳，客户端把待执行指令放在**响应体**里捎回去。
//! 一次请求同时完成「上报状态」与「取回指令」两件事。
//!
//! # ack 机制为何不可省
//! 心跳是轮询的，同一条指令会被反复取走。若没有 ack，插件执行 reload 后
//! 重新心跳又拿到同一条 reload，会陷入无限重载。故指令带 id，插件执行完
//! 在下次心跳带回 `acked`，客户端据此出队。
//!
//! # 自愈分级（判定在本地做，不绕中心）
//! 一级：心跳超时 → 下发 reload，插件自己调 runtime.reload()（无感恢复）
//! 二级：心跳彻底断 → 客户端重启 Chrome 并模拟快捷键拉起侧边栏
//!
//! 二级代价大（用户标签页会重开），故必须有冷却期与次数上限，否则某台机器
//! 因网络异常长期收不到心跳时会陷入反复重启 Chrome——那比插件卡死严重得多。

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// 一级自愈阈值：超过该时长未收到心跳，判定插件「半死」，下发 reload。
/// 取 20 秒是为容忍 3~4 次心跳丢失（插件侧 5 秒一次），避免长任务或
/// 短暂卡顿被误判——与插件内部 background→sidepanel 心跳的容忍口径一致
pub const RELOAD_THRESHOLD: Duration = Duration::from_secs(20);

/// 二级自愈阈值：超过该时长仍无心跳，判定插件「全死」（SW 崩溃到唤不醒，
/// 已取不走 reload 指令），升级为重启 Chrome
pub const RESTART_THRESHOLD: Duration = Duration::from_secs(120);

/// 重启 Chrome 的冷却期：一次重启后至少间隔这么久才允许再次重启。
/// 防止「重启→插件仍未上报→再重启」的死循环
pub const RESTART_COOLDOWN: Duration = Duration::from_secs(30 * 60);

/// 重启 Chrome 的累计次数上限。达到上限后不再重启，只记日志等人工介入——
/// 连续多次重启都救不回来，说明不是插件卡死而是别的问题，继续重启无意义
pub const MAX_RESTARTS: u32 = 3;

/// 插件上报的心跳（字段用驼峰，与插件侧 TypeScript 一致）
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HeartbeatRequest {
    /// 插件名，标识哪台机器/哪个采购账号
    #[serde(default)]
    pub plugin_name: Option<String>,
    /// 侧边栏当前是否打开。插件要求 sidepanel 常驻才能处理任务，
    /// 故「插件活着但侧边栏关了」也属需要介入的状态
    #[serde(default)]
    pub sidepanel_open: Option<bool>,
    /// 是否有任务正在执行。有任务时不应打断，reload 需延后
    #[serde(default)]
    pub task_running: Option<bool>,
    /// 业务 WebSocket 是否连接（插件侧 WS_CONNECTED）。
    /// WS 断开即收不到任务，实例白跑——这是开发日常巡检必查的一项
    #[serde(default)]
    pub ws_connected: Option<bool>,
    /// 1688 是否已登录。登出后任务全部失败，同为日常巡检必查项
    #[serde(default)]
    pub login_1688: Option<bool>,
    /// 接口返回的「应绑定 1688 账号」，与 `actual_account` 比对可发现串号
    #[serde(default)]
    pub expected_account: Option<String>,
    /// 实际登录的 1688 账号。与应绑定账号不一致意味着登错号，
    /// 会导致询价发给错误的供应商账号
    #[serde(default)]
    pub actual_account: Option<String>,
    /// 上一轮已执行完的指令 id，客户端据此出队
    #[serde(default)]
    pub acked: Vec<String>,
}

/// 下发给插件的指令
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Command {
    pub id: String,
    /// 指令类型。当前仅 "reload"，后续可扩展（立即上报日志、改配置等）
    #[serde(rename = "type")]
    pub kind: String,
}

/// 心跳响应：把待执行指令捎回给插件
#[derive(Debug, Serialize)]
pub struct HeartbeatResponse {
    pub commands: Vec<Command>,
}

/// 单个插件的运行状态
#[derive(Debug)]
pub struct PluginState {
    pub last_seen: Instant,
    pub sidepanel_open: bool,
    pub task_running: bool,
    /// 业务 WS 是否连接（None = 插件未上报，老版本插件兼容）
    pub ws_connected: Option<bool>,
    /// 1688 是否已登录（None = 未上报）
    pub login_1688: Option<bool>,
    /// 应绑定的 1688 账号
    pub expected_account: Option<String>,
    /// 实际登录的 1688 账号
    pub actual_account: Option<String>,
    /// 待下发指令队列（已下发但未收到 ack 的仍留在队列里，故会重复下发）
    pub pending: Vec<Command>,
    /// 已下发 reload 但插件尚未恢复心跳时，避免每轮都重复入队
    pub reload_issued: bool,
    /// 累计重启 Chrome 次数
    pub restart_count: u32,
    /// 最近一次重启 Chrome 的时刻，用于冷却期判定
    pub last_restart: Option<Instant>,
    /// 上一次已上报过的抑制原因。巡检每 5 秒一轮，若不去重，一台机器挂一天
    /// 会攒出上千条相同日志把有用信息淹掉（同类问题此前在自更新失败日志上
    /// 已治过一次）。原因变化时仍会重新记一条
    pub last_suppress_reason: Option<&'static str>,
    /// 是否已就「侧边栏未打开」记过日志。巡检每 5 秒一轮，不去重会刷屏
    pub sidepanel_closed_reported: bool,
}

impl PluginState {
    pub fn new(now: Instant) -> Self {
        Self {
            last_seen: now,
            sidepanel_open: false,
            task_running: false,
            ws_connected: None,
            login_1688: None,
            expected_account: None,
            actual_account: None,
            pending: Vec::new(),
            reload_issued: false,
            restart_count: 0,
            last_restart: None,
            last_suppress_reason: None,
            sidepanel_closed_reported: false,
        }
    }
}

/// 自愈决策：巡检某个插件状态后应采取的动作
#[derive(Debug, PartialEq)]
pub enum HealAction {
    /// 一切正常，无需处理
    None,
    /// 一级：下发 reload 指令（插件还能取走）
    IssueReload,
    /// 二级：重启 Chrome 并拉起侧边栏
    RestartChrome,
    /// 需要重启但被条件挡住，首次出现该原因时返回，调用方应记一条日志
    RestartSuppressed(&'static str),
    /// 同一抑制原因已上报过，调用方不必重复记日志。
    /// 巡检每 5 秒一轮，不去重会让一台故障机一天攒出上千条相同日志
    RestartSuppressedSilently,
    /// 插件活着但侧边栏没开。
    ///
    /// # 为什么只上报、不自动拉起（2026-08-21 止血）
    /// 拉起侧边栏只能靠「AppActivate 抢焦点 + SendKeys 发按键」，而**焦点是
    /// 全局唯一资源**：一次抢焦点会打断那台机器上全部实例里正在输入的那个，
    /// 不只是目标实例。插件正往供应商聊天框打字时被抢走焦点，按键会落进
    /// 输入框变成乱字符、或造成输入丢字——那是往外发出去的聊天内容被污染，
    /// 比侧边栏没开严重得多。
    ///
    /// 加节流/冷却都只是降低频率、不改变性质，故整条自动路径停用，
    /// 改为只记录、等人工处理。
    SidepanelClosed,
    /// 侧边栏未开且已上报过，不重复记日志（同为每 5 秒一轮的去重需要）
    SidepanelClosedSilently,
}

/// 单个实例的巡检快照，供客户端界面展示（DEV-125034）。
///
/// # 为什么需要它
/// 开发原本每天要逐个点开 15 个浏览器，逐一查看 WS 是否连接、1688 是否
/// 登出、窗口是否被最小化。这些判断插件内部本来就在做（badge 巡检、
/// StatusBar 账号比对），只是各自展示在自己的 sidepanel/badge 里。
/// 客户端本就在接收全部实例的心跳，汇总本该由它做。
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PluginSnapshot {
    pub plugin_name: String,
    /// 距上次心跳的秒数。越大越可疑
    pub silence_secs: u64,
    /// 是否已判定失联（超过一级阈值）
    pub stale: bool,
    pub sidepanel_open: bool,
    pub task_running: bool,
    pub ws_connected: Option<bool>,
    pub login_1688: Option<bool>,
    pub expected_account: Option<String>,
    pub actual_account: Option<String>,
    /// 账号是否串号：两个账号都已上报且不一致。
    /// 单独给出而非让前端比对，是因为「未上报」与「不一致」必须区分开
    pub account_mismatch: bool,
    /// 是否存在任一异常，供界面标红置顶
    pub has_issue: bool,
}

/// 心跳状态表。多个插件（理论上一台机器一个，但不假设唯一）各自独立计数
#[derive(Debug, Default)]
pub struct HeartbeatRegistry {
    plugins: HashMap<String, PluginState>,
    /// 自增序号，用于生成指令 id
    seq: u64,
}

impl HeartbeatRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// 处理一次心跳：更新状态、消费 ack、返回待下发指令。
    ///
    /// 收到心跳即说明插件活着，故清除 `reload_issued` 标记——下次真的超时时
    /// 才会重新入队，避免一次异常后永久抑制后续 reload
    pub fn on_heartbeat(
        &mut self,
        req: &HeartbeatRequest,
        now: Instant,
    ) -> HeartbeatResponse {
        let name = req
            .plugin_name
            .clone()
            .unwrap_or_else(|| "unknown".to_string());
        let state = self
            .plugins
            .entry(name)
            .or_insert_with(|| PluginState::new(now));

        state.last_seen = now;
        state.sidepanel_open = req.sidepanel_open.unwrap_or(false);
        state.task_running = req.task_running.unwrap_or(false);
        // 巡检字段：仅在插件上报时覆盖，未上报（老版本插件）保留上次值而非
        // 清成 None——否则新旧版本插件混跑时看板会闪烁
        if req.ws_connected.is_some() {
            state.ws_connected = req.ws_connected;
        }
        if req.login_1688.is_some() {
            state.login_1688 = req.login_1688;
        }
        if req.expected_account.is_some() {
            state.expected_account = req.expected_account.clone();
        }
        if req.actual_account.is_some() {
            state.actual_account = req.actual_account.clone();
        }

        // 收到心跳即说明插件活着，解除失联抑制标记。
        //
        // 原先只在收到 ack 时才清除，那是「自动下发 reload」年代的写法：
        // 插件 ack 了才算真重载过。停掉自动下发后（2026-08-24）永远收不到 ack，
        // 标记会永久生效——同一台机器只会上报一次失联、之后再也不报，
        // 反复出问题的机器在日志里只留得下最早那一条。
        state.reload_issued = false;

        // 消费 ack：插件已执行完的指令出队（手动下发的指令仍走这条路）
        if !req.acked.is_empty() {
            state.pending.retain(|c| !req.acked.contains(&c.id));
        }

        HeartbeatResponse {
            commands: state.pending.clone(),
        }
    }

    /// 生成全部实例的巡检快照，异常的排在前面。
    ///
    /// 排序规则：有问题的优先，其次按静默时长倒序，最后按名字——
    /// 15 行表格里，需要处理的必须一眼看到，不该让人自己扫。
    pub fn snapshots(&self, now: Instant) -> Vec<PluginSnapshot> {
        let mut list: Vec<PluginSnapshot> = self
            .plugins
            .iter()
            .map(|(name, s)| {
                let silence = now.saturating_duration_since(s.last_seen);
                let stale = silence >= RELOAD_THRESHOLD;
                // 账号串号：两边都上报了才比对。任一为空是「没上报」，
                // 不能当成「不一致」——那会让老版本插件全部误报串号
                let account_mismatch = match (&s.expected_account, &s.actual_account) {
                    (Some(e), Some(a)) if !e.is_empty() && !a.is_empty() => e != a,
                    _ => false,
                };
                let has_issue = stale
                    || !s.sidepanel_open
                    || s.ws_connected == Some(false)
                    || s.login_1688 == Some(false)
                    || account_mismatch;
                PluginSnapshot {
                    plugin_name: name.clone(),
                    silence_secs: silence.as_secs(),
                    stale,
                    sidepanel_open: s.sidepanel_open,
                    task_running: s.task_running,
                    ws_connected: s.ws_connected,
                    login_1688: s.login_1688,
                    expected_account: s.expected_account.clone(),
                    actual_account: s.actual_account.clone(),
                    account_mismatch,
                    has_issue,
                }
            })
            .collect();
        list.sort_by(|a, b| {
            b.has_issue
                .cmp(&a.has_issue)
                .then(b.silence_secs.cmp(&a.silence_secs))
                .then(a.plugin_name.cmp(&b.plugin_name))
        });
        list
    }

    /// 给指定实例入队一条指令（供界面手动下发，如重连 WS）。
    ///
    /// 返回指令 id；实例不存在时返回 None——不为未知实例建状态，
    /// 否则界面上一次误操作就会凭空多出一行僵尸记录
    pub fn enqueue_command(&mut self, plugin_name: &str, kind: &str) -> Option<String> {
        let id = format!("cmd-{}", self.seq);
        let state = self.plugins.get_mut(plugin_name)?;
        // 同类指令已在队列里就不重复入队：连点两次按钮不该让插件重连两次
        if state.pending.iter().any(|c| c.kind == kind) {
            return None;
        }
        state.pending.push(Command {
            id: id.clone(),
            kind: kind.to_string(),
        });
        self.seq += 1;
        Some(id)
    }

    /// 巡检所有插件，返回每个插件应采取的动作。
    /// 由后台定时任务调用；实际执行副作用（重启 Chrome 等）在调用方，
    /// 本函数只做判定，保持可测
    pub fn inspect(&mut self, now: Instant) -> Vec<(String, HealAction)> {
        // 一台机器会多开 Chrome（--user-data-dir 或多 Profile），每个实例跑一个
        // 插件、登录不同 CJ 账号。重启 Chrome 会把所有实例一起干掉，故先判断是否
        // 「全部失联」：只要还有插件在正常心跳，就说明 Chrome 本身没问题，
        // 单个插件的故障不该用重启浏览器解决，否则误伤其它账号正在跑的任务
        let all_silent = self
            .plugins
            .values()
            .all(|s| now.saturating_duration_since(s.last_seen) >= RESTART_THRESHOLD);

        let mut actions = Vec::new();
        // 一轮巡检最多重启一次：多个插件同时失联时，重启一次就够了，
        // 逐个触发会连环杀 Chrome
        let mut restart_used = false;
        // 先收集名单，避免在遍历中借用冲突
        let names: Vec<String> = self.plugins.keys().cloned().collect();
        for name in names {
            let action = self.decide(&name, now, all_silent, &mut restart_used);
            actions.push((name, action));
        }
        actions
    }

    /// 对单个插件做自愈判定，并在决定动作后同步更新其状态。
    ///
    /// # Arguments
    /// * `all_silent` - 是否所有插件都已失联。为 false 时不允许重启 Chrome，
    ///   因为还有插件活着说明浏览器本身没问题（多开场景下会误伤其它账号）
    /// * `restart_used` - 本轮巡检是否已触发过重启，避免多插件同时失联时连环杀 Chrome
    fn decide(
        &mut self,
        name: &str,
        now: Instant,
        all_silent: bool,
        restart_used: &mut bool,
    ) -> HealAction {
        let seq = self.seq;
        let state = match self.plugins.get_mut(name) {
            Some(s) => s,
            None => return HealAction::None,
        };
        let silence = now.saturating_duration_since(state.last_seen);

        // 二级：彻底失联，插件已取不走指令
        if silence >= RESTART_THRESHOLD {
            let suppress = if !all_silent {
                Some("其它插件仍存活，重启会误伤")
            } else if *restart_used {
                Some("本轮已重启过一次")
            } else if state.restart_count >= MAX_RESTARTS {
                Some("已达重启次数上限")
            } else if state
                .last_restart
                .is_some_and(|last| now.saturating_duration_since(last) < RESTART_COOLDOWN)
            {
                Some("处于重启冷却期内")
            } else {
                None
            };

            if let Some(reason) = suppress {
                // 同一原因只上报一次，避免每轮巡检刷同样的日志
                if state.last_suppress_reason == Some(reason) {
                    return HealAction::RestartSuppressedSilently;
                }
                state.last_suppress_reason = Some(reason);
                return HealAction::RestartSuppressed(reason);
            }

            state.restart_count += 1;
            state.last_restart = Some(now);
            state.last_suppress_reason = None;
            *restart_used = true;
            return HealAction::RestartChrome;
        }

        // 已脱离二级区间：清除抑制记录，下次真的失联时能重新上报
        state.last_suppress_reason = None;

        // 一级：疑似半死。**本期只上报、不自动下发 reload**（2026-08-24）。
        //
        // # 为什么停掉自动下发
        // 本期范围收窄为「只把日志链路跑通」，客户端不做任何自愈操作。而线上
        // 插件（release 分支）根本没有指令处理代码——它连 heartbeat.ts 都没有，
        // 那些改动全在 feat/client-heartbeat-selfheal 分支上、从未上线。
        //
        // 若这里照旧入队，指令会发给一个不会处理它的插件：未知类型被忽略、
        // 永远收不到 ack，队列里的 Command 就一直堆着，每次心跳都白传一遍。
        //
        // # 恢复条件
        // 插件侧的指令处理合入 release 之后，把下面注释掉的入队逻辑放回来即可。
        // 届时仍要注意两点（原分析保留，避免后人重踩）：
        // - **不在这里判 sidepanel_open**：触发本分支的前提是「心跳已超时」，
        //   手上的 sidepanel_open 是至少 20 秒前的旧值，据它决策等于拿过期数据
        //   下判断。插件真收到指令时在本地复检一次才准（插件侧已有该守卫）。
        // - **ack 对 reload 无效**：插件执行 reload 会销毁自己的 Service Worker、
        //   连带销毁待 ack 队列，那条 cmd id 永远发不回来。防重复只能靠
        //   reload_issued 标志，不能指望 ack 出队。
        if silence >= RELOAD_THRESHOLD {
            if state.reload_issued {
                return HealAction::None;
            }
            state.reload_issued = true;
            // 暂不入队，仅上报供观察真实发生频率：
            // state.pending.push(Command { id: format!("cmd-{}", seq), kind: "reload".into() });
            // self.seq += 1;
            let _ = seq;
            return HealAction::IssueReload;
        }

        // 插件活着但侧边栏关了：只上报，不自动拉起（原因见 SidepanelClosed 文档）
        if !state.sidepanel_open {
            if state.sidepanel_closed_reported {
                return HealAction::SidepanelClosedSilently;
            }
            state.sidepanel_closed_reported = true;
            return HealAction::SidepanelClosed;
        }
        // 侧边栏已恢复：清除标记，下次再关掉时能重新上报一次
        state.sidepanel_closed_reported = false;

        HealAction::None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(name: &str, sidepanel: bool, acked: Vec<String>) -> HeartbeatRequest {
        HeartbeatRequest {
            plugin_name: Some(name.to_string()),
            sidepanel_open: Some(sidepanel),
            task_running: Some(false),
            ws_connected: None,
            login_1688: None,
            expected_account: None,
            actual_account: None,
            acked,
        }
    }

    /// 带巡检字段的心跳，用于看板相关用例
    fn patrol_req(
        name: &str,
        ws: Option<bool>,
        login: Option<bool>,
        expected: Option<&str>,
        actual: Option<&str>,
    ) -> HeartbeatRequest {
        HeartbeatRequest {
            plugin_name: Some(name.to_string()),
            sidepanel_open: Some(true),
            task_running: Some(false),
            ws_connected: ws,
            login_1688: login,
            expected_account: expected.map(|s| s.to_string()),
            actual_account: actual.map(|s| s.to_string()),
            acked: vec![],
        }
    }

    #[test]
    fn test_snapshot_reports_patrol_fields() {
        let mut reg = HeartbeatRegistry::new();
        let now = Instant::now();
        reg.on_heartbeat(
            &patrol_req("robot-01", Some(true), Some(true), Some("acc-a"), Some("acc-a")),
            now,
        );

        let snaps = reg.snapshots(now);
        assert_eq!(snaps.len(), 1);
        let s = &snaps[0];
        assert_eq!(s.ws_connected, Some(true));
        assert_eq!(s.login_1688, Some(true));
        assert!(!s.account_mismatch);
        assert!(!s.has_issue, "各项正常时不应标记为异常");
    }

    #[test]
    fn test_snapshot_flags_ws_disconnected_as_issue() {
        let mut reg = HeartbeatRegistry::new();
        let now = Instant::now();
        reg.on_heartbeat(&patrol_req("robot-01", Some(false), Some(true), None, None), now);

        let s = &reg.snapshots(now)[0];
        assert!(
            s.has_issue,
            "WS 断开必须标记为异常——断开即收不到任务，实例白跑"
        );
    }

    #[test]
    fn test_snapshot_flags_logged_out_as_issue() {
        let mut reg = HeartbeatRegistry::new();
        let now = Instant::now();
        reg.on_heartbeat(&patrol_req("robot-01", Some(true), Some(false), None, None), now);

        assert!(
            reg.snapshots(now)[0].has_issue,
            "1688 登出必须标记为异常——登出后任务全部失败"
        );
    }

    #[test]
    fn test_snapshot_detects_account_mismatch() {
        let mut reg = HeartbeatRegistry::new();
        let now = Instant::now();
        reg.on_heartbeat(
            &patrol_req("robot-01", Some(true), Some(true), Some("acc-a"), Some("acc-b")),
            now,
        );

        let s = &reg.snapshots(now)[0];
        assert!(
            s.account_mismatch,
            "应绑定与实际登录账号不一致必须报出——登错号会把询价发给错误的供应商账号"
        );
        assert!(s.has_issue);
    }

    #[test]
    fn test_snapshot_missing_account_is_not_mismatch() {
        // 老版本插件不上报账号字段，不能因此全部误报串号
        let mut reg = HeartbeatRegistry::new();
        let now = Instant::now();
        reg.on_heartbeat(&patrol_req("robot-01", Some(true), Some(true), None, None), now);

        let s = &reg.snapshots(now)[0];
        assert!(
            !s.account_mismatch,
            "账号未上报应视为「不知道」而非「不一致」，否则老版本插件会全部误报"
        );
        assert!(!s.has_issue);
    }

    #[test]
    fn test_snapshot_puts_problem_instances_first() {
        // 15 行表格里需要处理的必须一眼看到
        let mut reg = HeartbeatRegistry::new();
        let now = Instant::now();
        reg.on_heartbeat(&patrol_req("robot-01", Some(true), Some(true), None, None), now);
        reg.on_heartbeat(&patrol_req("robot-02", Some(false), Some(true), None, None), now);
        reg.on_heartbeat(&patrol_req("robot-03", Some(true), Some(true), None, None), now);

        let snaps = reg.snapshots(now);
        assert_eq!(
            snaps[0].plugin_name, "robot-02",
            "异常实例必须排在最前，实际顺序: {:?}",
            snaps.iter().map(|s| &s.plugin_name).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_snapshot_marks_stale_instance() {
        let mut reg = HeartbeatRegistry::new();
        let t0 = Instant::now();
        reg.on_heartbeat(&patrol_req("robot-01", Some(true), Some(true), None, None), t0);

        let snaps = reg.snapshots(t0 + RELOAD_THRESHOLD);
        assert!(snaps[0].stale, "超过一级阈值应标记 stale");
        assert!(snaps[0].has_issue);
        assert_eq!(snaps[0].silence_secs, RELOAD_THRESHOLD.as_secs());
    }

    #[test]
    fn test_patrol_fields_not_cleared_when_omitted() {
        // 新旧版本插件混跑时，未上报的字段应保留上次值而非清空，否则看板闪烁
        let mut reg = HeartbeatRegistry::new();
        let t0 = Instant::now();
        reg.on_heartbeat(&patrol_req("robot-01", Some(true), Some(true), None, None), t0);
        // 老版本格式的心跳（不带巡检字段）
        reg.on_heartbeat(&req("robot-01", true, vec![]), t0 + Duration::from_secs(5));

        let s = &reg.snapshots(t0 + Duration::from_secs(5))[0];
        assert_eq!(
            s.ws_connected,
            Some(true),
            "未上报的巡检字段应保留上次值，不应被清成 None"
        );
    }

    #[test]
    fn test_enqueue_command_delivers_on_next_heartbeat() {
        let mut reg = HeartbeatRegistry::new();
        let now = Instant::now();
        reg.on_heartbeat(&req("robot-01", true, vec![]), now);

        let id = reg
            .enqueue_command("robot-01", "reconnectWs")
            .expect("已知实例应能入队指令");
        let resp = reg.on_heartbeat(&req("robot-01", true, vec![]), now);
        assert_eq!(resp.commands.len(), 1);
        assert_eq!(resp.commands[0].kind, "reconnectWs");
        assert_eq!(resp.commands[0].id, id);
    }

    #[test]
    fn test_enqueue_command_rejects_unknown_plugin() {
        let mut reg = HeartbeatRegistry::new();
        assert!(
            reg.enqueue_command("robot-99", "reconnectWs").is_none(),
            "不得为未知实例建状态，否则界面误操作会凭空多出僵尸记录"
        );
    }

    #[test]
    fn test_enqueue_command_deduplicates_same_kind() {
        let mut reg = HeartbeatRegistry::new();
        let now = Instant::now();
        reg.on_heartbeat(&req("robot-01", true, vec![]), now);

        assert!(reg.enqueue_command("robot-01", "reconnectWs").is_some());
        assert!(
            reg.enqueue_command("robot-01", "reconnectWs").is_none(),
            "同类指令已在队列时不应重复入队——连点两次按钮不该让插件重连两次"
        );
    }

    #[test]
    fn test_heartbeat_registers_plugin_and_returns_no_command_initially() {
        let mut reg = HeartbeatRegistry::new();
        let now = Instant::now();
        let resp = reg.on_heartbeat(&req("robot-01", true, vec![]), now);
        assert!(
            resp.commands.is_empty(),
            "首次心跳且状态正常时不应下发任何指令"
        );
    }

    #[test]
    fn test_silence_beyond_reload_threshold_issues_reload() {
        let mut reg = HeartbeatRegistry::new();
        let t0 = Instant::now();
        reg.on_heartbeat(&req("robot-01", true, vec![]), t0);

        let actions = reg.inspect(t0 + RELOAD_THRESHOLD);
        assert_eq!(
            actions[0].1,
            HealAction::IssueReload,
            "超过一级阈值应下发 reload——插件半死时靠它无感恢复"
        );
    }

    #[test]
    fn test_reload_is_reported_but_not_auto_issued() {
        // 本期（2026-08-24）只上报、不自动下发：线上插件没有指令处理代码，
        // 发过去只会被忽略且永远收不到 ack，指令在队列里一直堆着白传
        let mut reg = HeartbeatRegistry::new();
        let t0 = Instant::now();
        reg.on_heartbeat(&req("robot-01", true, vec![]), t0);

        let actions = reg.inspect(t0 + RELOAD_THRESHOLD);
        assert_eq!(
            actions[0].1,
            HealAction::IssueReload,
            "仍应上报，供观察真实发生频率"
        );

        let resp = reg.on_heartbeat(&req("robot-01", true, vec![]), t0 + RELOAD_THRESHOLD);
        assert!(
            resp.commands.is_empty(),
            "不得自动下发 reload——插件侧的指令处理尚未合入 release"
        );
    }

    #[test]
    fn test_acked_command_is_removed_from_queue() {
        let mut reg = HeartbeatRegistry::new();
        let t0 = Instant::now();
        reg.on_heartbeat(&req("robot-01", true, vec![]), t0);
        // 用手动下发造数据：巡检已不再自动入队（见 decide 的一级分支）
        reg.enqueue_command("robot-01", "reconnectWs")
            .expect("手动下发应成功");
        let resp = reg.on_heartbeat(&req("robot-01", true, vec![]), t0 + RELOAD_THRESHOLD);
        let cmd_id = resp.commands[0].id.clone();

        // 插件执行完并 ack 后，该指令必须出队
        let resp2 = reg.on_heartbeat(
            &req("robot-01", true, vec![cmd_id]),
            t0 + RELOAD_THRESHOLD + Duration::from_secs(1),
        );
        assert!(
            resp2.commands.is_empty(),
            "已 ack 的指令必须出队；否则插件会反复收到同一条 reload 陷入无限重载"
        );
    }

    #[test]
    fn test_reload_not_reissued_while_still_silent() {
        let mut reg = HeartbeatRegistry::new();
        let t0 = Instant::now();
        reg.on_heartbeat(&req("robot-01", true, vec![]), t0);

        reg.inspect(t0 + RELOAD_THRESHOLD);
        let second = reg.inspect(t0 + RELOAD_THRESHOLD + Duration::from_secs(5));
        assert_eq!(
            second[0].1,
            HealAction::None,
            "插件仍未恢复时不应每轮巡检都堆一条 reload，否则队列会无限增长"
        );
    }

    #[test]
    fn test_silence_beyond_restart_threshold_restarts_chrome() {
        let mut reg = HeartbeatRegistry::new();
        let t0 = Instant::now();
        reg.on_heartbeat(&req("robot-01", true, vec![]), t0);

        let actions = reg.inspect(t0 + RESTART_THRESHOLD);
        assert_eq!(
            actions[0].1,
            HealAction::RestartChrome,
            "彻底失联时插件已取不走 reload，必须升级为重启 Chrome"
        );
    }

    #[test]
    fn test_restart_is_suppressed_within_cooldown() {
        let mut reg = HeartbeatRegistry::new();
        let t0 = Instant::now();
        reg.on_heartbeat(&req("robot-01", true, vec![]), t0);
        reg.inspect(t0 + RESTART_THRESHOLD);

        // 冷却期内再次巡检不应重启
        let again = reg.inspect(t0 + RESTART_THRESHOLD + Duration::from_secs(60));
        assert!(
            matches!(again[0].1, HealAction::RestartSuppressed(_)),
            "冷却期内必须抑制重启，否则会陷入反复重启 Chrome——比插件卡死更严重"
        );
    }

    #[test]
    fn test_restart_is_suppressed_after_max_attempts() {
        let mut reg = HeartbeatRegistry::new();
        let t0 = Instant::now();
        reg.on_heartbeat(&req("robot-01", true, vec![]), t0);

        // 每次都跨过冷却期，重启到上限
        let mut t = t0;
        for _ in 0..MAX_RESTARTS {
            t += RESTART_THRESHOLD + RESTART_COOLDOWN;
            let acts = reg.inspect(t);
            assert_eq!(acts[0].1, HealAction::RestartChrome);
        }
        t += RESTART_THRESHOLD + RESTART_COOLDOWN;
        let acts = reg.inspect(t);
        assert!(
            matches!(acts[0].1, HealAction::RestartSuppressed(_)),
            "达到次数上限后应停手等人工介入——连续重启都救不回说明不是插件卡死"
        );
    }

    #[test]
    fn test_closed_sidepanel_is_reported_not_auto_opened() {
        // 侧边栏未开只上报、不自动拉起：拉起要抢全局焦点+模拟按键，会打断
        // 那台机器上正在往供应商聊天框输入文字的实例，污染发出去的内容
        let mut reg = HeartbeatRegistry::new();
        let now = Instant::now();
        reg.on_heartbeat(&req("robot-01", false, vec![]), now);

        let actions = reg.inspect(now);
        assert_eq!(
            actions[0].1,
            HealAction::SidepanelClosed,
            "侧边栏未开应只上报等人工处理，不得返回「去拉起」这种会抢焦点的动作"
        );
    }

    #[test]
    fn test_closed_sidepanel_reported_only_once() {
        // 巡检每 5 秒一轮，同一状态若每轮都记日志，一天会攒出上千条相同内容
        let mut reg = HeartbeatRegistry::new();
        let t0 = Instant::now();
        reg.on_heartbeat(&req("robot-01", false, vec![]), t0);

        assert_eq!(reg.inspect(t0).swap_remove(0).1, HealAction::SidepanelClosed);

        let t1 = t0 + Duration::from_secs(5);
        reg.on_heartbeat(&req("robot-01", false, vec![]), t1);
        assert_eq!(
            reg.inspect(t1).swap_remove(0).1,
            HealAction::SidepanelClosedSilently,
            "同一状态重复出现时应静默，避免日志刷屏"
        );
    }

    #[test]
    fn test_closed_sidepanel_reported_again_after_recovery() {
        // 侧边栏恢复后又关掉，应重新上报一次——否则只能在日志里看到最早那条
        let mut reg = HeartbeatRegistry::new();
        let t0 = Instant::now();
        reg.on_heartbeat(&req("robot-01", false, vec![]), t0);
        reg.inspect(t0);

        // 侧边栏打开 → 标记清除
        let t1 = t0 + Duration::from_secs(5);
        reg.on_heartbeat(&req("robot-01", true, vec![]), t1);
        assert_eq!(reg.inspect(t1).swap_remove(0).1, HealAction::None);

        // 再次关闭 → 应重新上报
        let t2 = t1 + Duration::from_secs(5);
        reg.on_heartbeat(&req("robot-01", false, vec![]), t2);
        assert_eq!(
            reg.inspect(t2).swap_remove(0).1,
            HealAction::SidepanelClosed,
            "恢复后再次关闭必须重新上报，否则后续故障在日志里看不出来"
        );
    }

    #[test]
    fn test_healthy_plugin_needs_no_action() {
        let mut reg = HeartbeatRegistry::new();
        let now = Instant::now();
        reg.on_heartbeat(&req("robot-01", true, vec![]), now);

        let actions = reg.inspect(now + Duration::from_secs(5));
        assert_eq!(
            actions[0].1,
            HealAction::None,
            "心跳正常且侧边栏已开时不应有任何动作"
        );
    }

    #[test]
    fn test_recovered_heartbeat_clears_reload_suppression() {
        let mut reg = HeartbeatRegistry::new();
        let t0 = Instant::now();
        reg.on_heartbeat(&req("robot-01", true, vec![]), t0);
        // 第一次超时：上报一次（reload_issued 置位，抑制后续重复上报）
        assert_eq!(
            reg.inspect(t0 + RELOAD_THRESHOLD)[0].1,
            HealAction::IssueReload
        );
        assert_eq!(
            reg.inspect(t0 + RELOAD_THRESHOLD + Duration::from_secs(5))[0].1,
            HealAction::None,
            "同一次失联期间不应重复上报"
        );

        // 插件恢复心跳后再次失联，应能重新上报——否则抑制标记会永久生效，
        // 一台机器反复出问题只会在日志里留下最早那一条
        let t1 = t0 + RELOAD_THRESHOLD + Duration::from_secs(10);
        reg.on_heartbeat(&req("robot-01", true, vec![]), t1);

        let actions = reg.inspect(t1 + RELOAD_THRESHOLD);
        assert_eq!(
            actions[0].1,
            HealAction::IssueReload,
            "恢复过一次后再失联必须能重新上报，否则抑制标记会永久生效"
        );
    }

    #[test]
    fn test_restart_suppressed_when_other_plugins_still_alive() {
        // 一台机器会多开 Chrome（--user-data-dir 或多 Profile），每个实例跑一个
        // 插件、登录不同 CJ 账号。重启 Chrome 会把所有实例一起干掉——只要还有
        // 插件在正常心跳，就说明 Chrome 本身是好的，单个插件的问题不该用重启
        // 浏览器来解决，否则会误伤其它账号正在跑的任务
        let mut reg = HeartbeatRegistry::new();
        let t0 = Instant::now();
        reg.on_heartbeat(&req("robot-01", true, vec![]), t0);
        reg.on_heartbeat(&req("robot-02", true, vec![]), t0);

        // robot-01 彻底失联，但 robot-02 一直正常心跳
        let t1 = t0 + RESTART_THRESHOLD;
        reg.on_heartbeat(&req("robot-02", true, vec![]), t1);

        let actions = reg.inspect(t1);
        let map: HashMap<_, _> = actions.into_iter().collect();
        assert!(
            matches!(map["robot-01"], HealAction::RestartSuppressed(_)),
            "还有其它插件存活时不得重启 Chrome，实际: {:?}",
            map["robot-01"]
        );
    }

    #[test]
    fn test_restart_allowed_when_all_plugins_are_silent() {
        // 全部插件都失联 → Chrome 整体出问题，此时重启不会误伤谁
        let mut reg = HeartbeatRegistry::new();
        let t0 = Instant::now();
        reg.on_heartbeat(&req("robot-01", true, vec![]), t0);
        reg.on_heartbeat(&req("robot-02", true, vec![]), t0);

        let actions = reg.inspect(t0 + RESTART_THRESHOLD);
        let map: HashMap<_, _> = actions.into_iter().collect();
        // 只需有插件真正触发重启即可（另一个会因冷却/已重启被抑制）
        let restarted = map.values().filter(|a| **a == HealAction::RestartChrome).count();
        assert_eq!(
            restarted, 1,
            "全部失联时应重启一次 Chrome（不是每个插件各重启一次），实际动作: {:?}",
            map
        );
    }

    #[test]
    fn test_repeated_suppression_is_reported_only_once() {
        // 巡检每 5 秒一轮，同一抑制原因若每轮都记日志，一台故障机一天会攒出
        // 上千条相同内容，把真正有用的线索淹掉
        let mut reg = HeartbeatRegistry::new();
        let t0 = Instant::now();
        reg.on_heartbeat(&req("robot-01", true, vec![]), t0);
        reg.on_heartbeat(&req("robot-02", true, vec![]), t0);

        let t1 = t0 + RESTART_THRESHOLD;
        reg.on_heartbeat(&req("robot-02", true, vec![]), t1);

        // 第一次应给出具体原因（供调用方记一条日志）
        let first: HashMap<_, _> = reg.inspect(t1).into_iter().collect();
        assert!(
            matches!(first["robot-01"], HealAction::RestartSuppressed(_)),
            "首次抑制应带原因，实际: {:?}",
            first["robot-01"]
        );

        // 紧接着的巡检（原因未变）应转为静默，不再要求记日志
        reg.on_heartbeat(&req("robot-02", true, vec![]), t1 + Duration::from_secs(5));
        let second: HashMap<_, _> = reg
            .inspect(t1 + Duration::from_secs(5))
            .into_iter()
            .collect();
        assert_eq!(
            second["robot-01"],
            HealAction::RestartSuppressedSilently,
            "同一原因重复出现时应静默，避免日志刷屏"
        );
    }

    #[test]
    fn test_suppression_reported_again_after_recovery() {
        // 插件恢复过又再次失联时，应重新上报一次——否则一台机器反复出问题
        // 只会在日志里留下最早那一条，看不出后续又发生过
        let mut reg = HeartbeatRegistry::new();
        let t0 = Instant::now();
        reg.on_heartbeat(&req("robot-01", true, vec![]), t0);
        reg.on_heartbeat(&req("robot-02", true, vec![]), t0);

        let t1 = t0 + RESTART_THRESHOLD;
        reg.on_heartbeat(&req("robot-02", true, vec![]), t1);
        reg.inspect(t1); // 首次抑制

        // robot-01 恢复心跳
        let t2 = t1 + Duration::from_secs(1);
        reg.on_heartbeat(&req("robot-01", true, vec![]), t2);
        reg.inspect(t2);

        // 再次失联，应重新给出原因
        let t3 = t2 + RESTART_THRESHOLD;
        reg.on_heartbeat(&req("robot-02", true, vec![]), t3);
        let again: HashMap<_, _> = reg.inspect(t3).into_iter().collect();
        assert!(
            matches!(again["robot-01"], HealAction::RestartSuppressed(_)),
            "恢复后再次失联必须重新上报，实际: {:?}",
            again["robot-01"]
        );
    }

    #[test]
    fn test_single_plugin_restart_still_works() {
        // 只有一个插件时，「全部失联」等价于「它失联」，不应被新规则挡住
        let mut reg = HeartbeatRegistry::new();
        let t0 = Instant::now();
        reg.on_heartbeat(&req("robot-01", true, vec![]), t0);

        let actions = reg.inspect(t0 + RESTART_THRESHOLD);
        assert_eq!(
            actions[0].1,
            HealAction::RestartChrome,
            "单插件场景不应被「其它插件存活」规则误伤"
        );
    }

    #[test]
    fn test_multiple_plugins_tracked_independently() {
        let mut reg = HeartbeatRegistry::new();
        let t0 = Instant::now();
        reg.on_heartbeat(&req("robot-01", true, vec![]), t0);
        // robot-02 晚 15 秒才上报，此时 robot-01 已静默 15 秒
        reg.on_heartbeat(&req("robot-02", true, vec![]), t0 + Duration::from_secs(15));

        let actions = reg.inspect(t0 + RELOAD_THRESHOLD);
        let map: HashMap<_, _> = actions.into_iter().collect();
        assert_eq!(
            map["robot-01"],
            HealAction::IssueReload,
            "robot-01 已超阈值应下发 reload"
        );
        assert_eq!(
            map["robot-02"],
            HealAction::None,
            "robot-02 心跳仍新鲜，不应被 robot-01 的状态波及"
        );
    }

    #[test]
    fn test_missing_plugin_name_falls_back_to_unknown() {
        let mut reg = HeartbeatRegistry::new();
        let now = Instant::now();
        let r = HeartbeatRequest {
            plugin_name: None,
            sidepanel_open: Some(true),
            task_running: Some(false),
            ws_connected: None,
            login_1688: None,
            expected_account: None,
            actual_account: None,
            acked: vec![],
        };
        reg.on_heartbeat(&r, now);
        let actions = reg.inspect(now);
        assert_eq!(
            actions[0].0, "unknown",
            "插件名缺失时回落 unknown，而非丢弃这次心跳"
        );
    }
}
