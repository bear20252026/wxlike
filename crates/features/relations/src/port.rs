//! 跨域只读端口：聊天/推送/搜索从这里问关系，**不许**自己查关系表。
//!
//! 一个 trait + 一个 SQL 实现。聊天域（G03）拿到 [`RelationsPort`] 之后，
//! 关系表结构怎么变都与它无关；反过来，本 crate 也不关心消息怎么存。
//! 这条边界就是"不拷贝对方业务算法"的落地方式。

use rusqlite::Connection;

use crate::group::{self, Role};
use crate::{friend, Result};

/// 一条消息能否发给对方。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MessageGate {
    /// 好友、或发给自己的设备（多端同步）。
    Allowed,
    /// 非好友：进"陌生人消息请求箱"，不产生会话。
    StrangerRequest,
    /// 任一方向被拉黑：直接拒绝，且不告诉发送方具体原因。
    Blocked,
}

pub trait RelationsPort {
    fn are_friends(&self, a: &str, b: &str) -> Result<bool>;
    fn is_blocked(&self, by: &str, target: &str) -> Result<bool>;
    fn may_message(&self, from: &str, to: &str) -> Result<MessageGate>;
    fn shared_groups(&self, a: &str, b: &str) -> Result<Vec<String>>;
    fn member_ids(&self, group_id: &str) -> Result<Vec<String>>;
    fn role_of(&self, group_id: &str, user: &str) -> Result<Option<Role>>;
}

/// 直接读真实关系表的实现（平台在请求作用域里构造，开销只有一次借用）。
pub struct SqlRelations<'a> {
    pub conn: &'a Connection,
}

impl RelationsPort for SqlRelations<'_> {
    fn are_friends(&self, a: &str, b: &str) -> Result<bool> {
        friend::are_friends(self.conn, a, b)
    }

    fn is_blocked(&self, by: &str, target: &str) -> Result<bool> {
        friend::is_blocked(self.conn, by, target)
    }

    fn may_message(&self, from: &str, to: &str) -> Result<MessageGate> {
        if friend::blocked_either(self.conn, from, to)? {
            return Ok(MessageGate::Blocked);
        }
        if from == to || friend::are_friends(self.conn, from, to)? {
            return Ok(MessageGate::Allowed);
        }
        Ok(MessageGate::StrangerRequest)
    }

    fn shared_groups(&self, a: &str, b: &str) -> Result<Vec<String>> {
        Ok(group::common_groups(self.conn, a, b)?
            .into_iter()
            .map(|g| g.id)
            .collect())
    }

    fn member_ids(&self, group_id: &str) -> Result<Vec<String>> {
        group::members(self.conn, group_id)
    }

    fn role_of(&self, group_id: &str, user: &str) -> Result<Option<Role>> {
        group::role_of(self.conn, group_id, user)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::group::{self, Visibility};
    use crate::testing::db;

    #[test]
    fn gate_allows_friends_and_blocks_blocked() {
        let conn = db();
        let port = SqlRelations { conn: &conn };
        assert_eq!(
            port.may_message("u1", "u2").unwrap(),
            MessageGate::StrangerRequest
        );
        friend::send(&conn, "u1", "u2", "", 1_000).unwrap();
        friend::send(&conn, "u2", "u1", "", 1_100).unwrap();
        assert_eq!(port.may_message("u1", "u2").unwrap(), MessageGate::Allowed);
        assert_eq!(port.may_message("u1", "u1").unwrap(), MessageGate::Allowed);
        friend::block(&conn, "u2", "u1", 1_200).unwrap();
        assert_eq!(port.may_message("u1", "u2").unwrap(), MessageGate::Blocked);
    }

    #[test]
    fn shared_groups_and_membership_via_port() {
        let conn = db();
        let g = group::create(
            &conn,
            "u1",
            "群",
            &["u2".to_string()],
            Visibility::Public,
            1_000,
        )
        .unwrap();
        let port = SqlRelations { conn: &conn };
        assert_eq!(port.shared_groups("u1", "u2").unwrap(), vec![g.id.clone()]);
        assert_eq!(port.member_ids(&g.id).unwrap().len(), 2);
        assert_eq!(port.role_of(&g.id, "u1").unwrap(), Some(Role::Owner));
        assert!(port.shared_groups("u1", "u9").unwrap().is_empty());
    }
}
