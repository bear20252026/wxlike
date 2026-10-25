//! 直播房间的角色门禁（G09-05）。
//!
//! [`SfuRoom`] 本身只回答"媒体能否转发、是否观众"；直播还需要把**谁可以发**
//! 绑定到主播/连麦名单上。`LiveRoom` 是这层薄封装：
//!
//! * 主播与连麦者才是 publisher；普通观众即使客户端偷偷 `write_audio`，
//!   也会在 SFU 的发布者过滤处被丢弃（见 `media_gates` 测试）；
//! * 连麦批准/取消直接切换发布权限，不需要重连；
//! * 停播立即清空全部参与者，释放 socket 与转发状态。

use std::collections::HashSet;

use crate::{JoinReply, SfuJoinError, SfuRoom};

#[derive(Debug, thiserror::Error)]
pub enum LiveRoomError {
    #[error("live room already stopped")]
    Stopped,
    #[error("session token belongs to another user")]
    IdentityMismatch,
    #[error(transparent)]
    Join(#[from] SfuJoinError),
}

/// 一个直播间的角色视图。`host` / `cohosts` 都是平台解析出的 user id。
pub struct LiveRoom {
    room: SfuRoom,
    host: String,
    cohosts: HashSet<String>,
    stopped: bool,
    stopped_viewers: usize,
}

impl LiveRoom {
    pub fn new(room: SfuRoom, host: impl Into<String>) -> Self {
        Self {
            room,
            host: host.into(),
            cohosts: HashSet::new(),
            stopped: false,
            stopped_viewers: 0,
        }
    }

    pub fn host(&self) -> &str {
        &self.host
    }

    pub fn cohosts(&self) -> Vec<&str> {
        self.cohosts.iter().map(String::as_str).collect()
    }

    pub fn is_cohost(&self, user: &str) -> bool {
        self.cohosts.contains(user)
    }

    pub fn is_stopped(&self) -> bool {
        self.stopped
    }

    pub fn viewer_connections(&self) -> usize {
        self.room.len()
    }

    pub fn media(&self) -> &SfuRoom {
        &self.room
    }

    pub fn media_mut(&mut self) -> &mut SfuRoom {
        &mut self.room
    }

    /// 主播推流加入。token 必须属于 `host`。
    pub fn join_host(
        &mut self,
        host: &str,
        token: &str,
        offer: serde_json::Value,
        video: bool,
    ) -> Result<JoinReply, LiveRoomError> {
        self.ensure_open()?;
        if host != self.host || self.room.resolve_user(token)? != self.host {
            return Err(LiveRoomError::IdentityMismatch);
        }
        Ok(self.room.join(token, offer, video)?)
    }

    /// 观众观看加入；永远不是 publisher。
    pub fn join_viewer(
        &mut self,
        token: &str,
        offer: serde_json::Value,
        video: bool,
    ) -> Result<JoinReply, LiveRoomError> {
        self.ensure_open()?;
        Ok(self.room.join_viewer(token, offer, video)?)
    }

    /// 连麦者加入（主播已批准）。token 必须属于 `user`。
    pub fn join_cohost(
        &mut self,
        user: &str,
        token: &str,
        offer: serde_json::Value,
        video: bool,
    ) -> Result<JoinReply, LiveRoomError> {
        self.ensure_open()?;
        if self.room.resolve_user(token)? != user {
            return Err(LiveRoomError::IdentityMismatch);
        }
        let reply = self.room.join(token, offer, video)?;
        self.cohosts.insert(user.to_string());
        self.room.set_publisher(user, true);
        Ok(reply)
    }

    /// 取消连麦：立即停止转发其上行，连接保留（他还可以继续看）。
    pub fn revoke_cohost(&mut self, user: &str) -> bool {
        self.cohosts.remove(user) && self.room.set_publisher(user, false)
    }

    /// 停播：断开所有连接并释放媒体资源。返回被断开的连接数。
    pub fn stop(&mut self) -> usize {
        if self.stopped {
            return self.stopped_viewers;
        }
        let users = self.room.participants();
        self.stopped_viewers = users.len();
        for user in users {
            self.room.leave(&user);
        }
        self.cohosts.clear();
        self.stopped = true;
        self.stopped_viewers
    }

    fn ensure_open(&self) -> Result<(), LiveRoomError> {
        if self.stopped {
            return Err(LiveRoomError::Stopped);
        }
        Ok(())
    }
}
