//! 常驻对话模式。桥靠 `metadata.mode` 区分单栏/双栏，所以这里的字符串必须和桥一致。

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Agent,
    Direct,
    Side,
    Battle,
}

impl Mode {
    /// 传给桥 `metadata.mode` 的字面量
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Agent => "agent",
            Mode::Direct => "direct",
            Mode::Side => "side",
            Mode::Battle => "battle",
        }
    }

    /// 状态栏中文名
    pub fn label(self) -> &'static str {
        match self {
            Mode::Agent => "agent",
            Mode::Direct => "direct",
            Mode::Side => "side",
            Mode::Battle => "battle",
        }
    }

    /// 中文说明，给 /help 和提示用
    pub fn describe(self) -> &'static str {
        match self {
            Mode::Agent => "Agent 模式：Arena 会自己跑工具、读文件",
            Mode::Direct => "Direct 模式：单个模型直接答，不跑工具",
            Mode::Side => "Side-by-side：两栏并排对比，回答合成一条",
            Mode::Battle => "Battle：两栏对战，投完票给你两边的答案",
        }
    }

    pub fn parse(s: &str) -> Option<Mode> {
        match s.trim().to_ascii_lowercase().as_str() {
            "agent" | "a" => Some(Mode::Agent),
            "direct" | "d" | "chat" => Some(Mode::Direct),
            "side" | "side-by-side" | "sbs" | "s" => Some(Mode::Side),
            "battle" | "b" | "compare" => Some(Mode::Battle),
            _ => None,
        }
    }

    /// 双栏模式（side / battle）
    pub fn is_dual(self) -> bool {
        matches!(self, Mode::Side | Mode::Battle)
    }

    /// 该模式下的默认模型名（模型列表只影响双栏两栏）
    pub fn default_model(self) -> &'static str {
        match self {
            Mode::Agent => "arena-agent",
            Mode::Direct => "arena-direct",
            Mode::Side => "arena-agent",
            Mode::Battle => "arena-agent",
        }
    }

    pub fn all() -> [Mode; 4] {
        [Mode::Agent, Mode::Direct, Mode::Side, Mode::Battle]
    }
}

impl fmt::Display for Mode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_aliases() {
        assert_eq!(Mode::parse("agent"), Some(Mode::Agent));
        assert_eq!(Mode::parse("AGENT"), Some(Mode::Agent));
        assert_eq!(Mode::parse("direct"), Some(Mode::Direct));
        assert_eq!(Mode::parse("side"), Some(Mode::Side));
        assert_eq!(Mode::parse("side-by-side"), Some(Mode::Side));
        assert_eq!(Mode::parse("battle"), Some(Mode::Battle));
        assert_eq!(Mode::parse("nonsense"), None);
        assert_eq!(Mode::parse(""), None);
    }

    #[test]
    fn wire_strings_match_bridge() {
        assert_eq!(Mode::Agent.as_str(), "agent");
        assert_eq!(Mode::Direct.as_str(), "direct");
        assert_eq!(Mode::Side.as_str(), "side");
        assert_eq!(Mode::Battle.as_str(), "battle");
    }

    #[test]
    fn dual_modes() {
        assert!(Mode::Side.is_dual());
        assert!(Mode::Battle.is_dual());
        assert!(!Mode::Agent.is_dual());
    }
}
