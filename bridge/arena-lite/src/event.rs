//! 主循环收到的消息。键盘、网络、健康检查都汇到这一个管道里。

use crate::client::{Health, History, StreamEvent};
use crate::err::BridgeError;
use crate::keys::Key;

/// 会话列表里的一项（/resume）
#[derive(Debug, Clone)]
pub struct SessionInfo {
    pub name: String,
    pub at: Option<i64>,
    pub last: String,
    pub turns: usize,
    pub error: Option<String>,
}

pub enum Msg {
    Key(Key),
    /// 某一次提问的流式增量（带代号，旧回合的残余直接丢掉）
    Stream { id: u64, ev: StreamEvent },
    /// 某一次提问结束（成功或失败）
    StreamEnd { id: u64, result: Result<(), BridgeError> },
    /// 需要人工验证：正文提示 + 终端铃 + 状态栏转红
    NeedVerify { id: u64 },
    /// /health 轮询结果
    Health(Result<Health, BridgeError>),
    /// /history 结果
    History(Result<History, BridgeError>),
    /// 会话列表（名字 + 摘要 + 时间）
    SessionList(Vec<SessionInfo>),
    /// 一次性提示（比如「已切到 side 模式」）
    Notice(String),
}
