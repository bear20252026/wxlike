//! Group membership lookup for the hub (WP-04).
//!
//! The hub must deliver a group message by the **member table** rather than the
//! scaffold's "empty `to` means broadcast to everyone connected" rule. It does
//! not own that table — `wxlike-platform` does — so it asks through this trait.
//!
//! Returning `None` means "not a group conversation", which keeps DM and
//! broadcast behaviour intact for code paths that predate groups.
//!
//! Implementations are expected to cache: the hub asks per message. The
//! platform's implementation caches in-process and invalidates on membership
//! change, which is what WP-04 A1's cache test exercises.

use async_trait::async_trait;

#[async_trait]
pub trait Membership: Send + Sync + 'static {
    /// User ids belonging to `conv_id`, or `None` when it is not a group.
    async fn members_of(&self, conv_id: &str) -> anyhow::Result<Option<Vec<String>>>;
}
