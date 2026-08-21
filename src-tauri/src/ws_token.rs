//! WebSocket 握手令牌（DEV-125034）
//!
//! # 为什么必须有这道校验
//! 通信从 HTTP 轮询改为 WS 长连接后，**任意网页脚本都能连
//! `ws://127.0.0.1:17653/ws`** —— WebSocket 不受同源策略限制（没有预检、
//! 浏览器也不会因跨域拦截）。若不校验来源，1688 页面上的任意脚本可以：
//!
//! - 伪装成插件上报假状态（让巡检看板显示一切正常，掩盖真实故障）
//! - 接收客户端下发的指令（指令里可能含配置信息）
//! - 占住连接槽位，把真插件挤掉
//!
//! 这与 CLAUDE.md 安全红线直接相关：`/log` 的 CORS 之所以能放开任意来源，
//! 前提是「只写不读」；WS 是**双向**的，那个前提不再成立，必须补校验。
//!
//! # 为什么用「本地文件共享令牌」而不是别的方案
//! 插件与客户端跑在同一台机器上，但插件在浏览器沙箱里读不到本地文件。
//! 故令牌由客户端生成并写入插件能拿到的位置——即插件的安装目录（客户端
//! 本来就在往那里解压插件包，见 perform_update）。网页脚本读不到扩展的
//! 私有文件，故只有真插件能拿到令牌。
//!
//! 令牌**每次客户端启动时重新生成**：不持久化，重启即失效，
//! 泄露的影响面限制在单次运行周期内。

use std::time::{SystemTime, UNIX_EPOCH};

/// 令牌长度（十六进制字符数）。16 字节熵 → 32 个 hex 字符，
/// 足够抵御在线暴力猜测（本服务只监听 127.0.0.1，无远程爆破面）
const TOKEN_HEX_LEN: usize = 32;

/// 生成一个随机令牌。
///
/// 用「时间戳 + 进程 id + 地址熵」混合而非引入 rand 依赖：本令牌只需
/// 「同一台机器上的其它网页脚本猜不到」，不用于加密。混合三个源是因为
/// 单用时间戳会被同一毫秒内启动的进程撞上，也容易被猜。
pub fn generate_token() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let pid = std::process::id() as u128;
    // 取一个栈变量地址作为第三个熵源（ASLR 下每次运行不同）
    let stack_marker = &nanos as *const u128 as u128;

    // 三段混合后取 hex，做一次简单扩散避免高位全是固定前缀
    let mut acc = nanos ^ (pid << 64) ^ stack_marker;
    let mut out = String::with_capacity(TOKEN_HEX_LEN);
    while out.len() < TOKEN_HEX_LEN {
        // xorshift 风格扩散：让每一轮输出都依赖全部输入位
        acc ^= acc << 13;
        acc ^= acc >> 7;
        acc ^= acc << 17;
        out.push_str(&format!("{:016x}", (acc & 0xffff_ffff_ffff_ffff) as u64));
    }
    out.truncate(TOKEN_HEX_LEN);
    out
}

/// 常量时间比较两个令牌。
///
/// 不用 `==` 是因为字符串比较会在首个不同字节处短路返回，理论上可被
/// 计时攻击逐字节试探。本服务只监听本机、攻击面小，但校验代码是安全
/// 边界的一部分，按正确做法写不增加成本。
pub fn token_matches(expected: &str, provided: &str) -> bool {
    if expected.len() != provided.len() {
        return false;
    }
    // 空令牌一律拒绝：避免「令牌未初始化」被当成「校验通过」
    if expected.is_empty() {
        return false;
    }
    let mut diff = 0u8;
    for (a, b) in expected.bytes().zip(provided.bytes()) {
        diff |= a ^ b;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generated_token_has_expected_length() {
        let t = generate_token();
        assert_eq!(
            t.len(),
            TOKEN_HEX_LEN,
            "令牌长度须固定，否则常量时间比较的长度检查会泄露信息"
        );
    }

    #[test]
    fn test_generated_token_is_hex_only() {
        let t = generate_token();
        assert!(
            t.chars().all(|c| c.is_ascii_hexdigit()),
            "令牌须为纯 hex，避免作为 URL 查询参数时需要转义: {}",
            t
        );
    }

    #[test]
    fn test_generated_tokens_differ_between_calls() {
        // 每次客户端启动都应拿到不同令牌——固定令牌等于没有令牌
        let a = generate_token();
        let b = generate_token();
        assert_ne!(a, b, "两次生成的令牌不应相同，否则可被预测");
    }

    #[test]
    fn test_token_matches_accepts_identical() {
        let t = generate_token();
        assert!(token_matches(&t, &t), "同一令牌必须校验通过");
    }

    #[test]
    fn test_token_matches_rejects_different() {
        assert!(
            !token_matches("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
            "不同令牌必须拒绝"
        );
    }

    #[test]
    fn test_token_matches_rejects_length_mismatch() {
        assert!(
            !token_matches("abcd", "abcde"),
            "长度不同必须拒绝，不得因前缀相同而通过"
        );
        assert!(
            !token_matches("abcde", "abcd"),
            "反向长度不同同样必须拒绝"
        );
    }

    #[test]
    fn test_token_matches_rejects_empty_expected() {
        // 令牌未初始化（空字符串）时，任何输入都不该通过——否则
        // 「忘记设置令牌」会静默变成「不校验」，这是最危险的失败模式
        assert!(
            !token_matches("", ""),
            "期望令牌为空时必须拒绝，避免未初始化被当成校验通过"
        );
        assert!(!token_matches("", "anything"), "空期望令牌不得接受任何输入");
    }

    #[test]
    fn test_token_matches_rejects_prefix() {
        let t = "0123456789abcdef0123456789abcdef";
        assert!(
            !token_matches(t, "0123456789abcdef"),
            "正确前缀不得通过——这正是长度检查存在的理由"
        );
    }
}
