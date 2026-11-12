//! **handler 层**（`groups`）：路由 + 提取器 + 响应组装。
//! 不碰 SQL——`crate::domain::repo` 对本模块不可见。

use crate::auth::Authed;
use crate::db::offload;
use crate::error::{ApiError, PlatformError};
use crate::extract::ApiJson;
use crate::validate;
use crate::PlatformState;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};

use crate::domain::groups::{group_notice, idempotency_key, GroupInfo, GroupSummary, MemberRow};

pub fn routes() -> Router<PlatformState> {
    Router::new()
        .route("/api/groups", post(create_group))
        .route("/api/groups/mine", get(my_groups))
        .route("/api/groups/{group_id}", get(group_info))
        .route("/api/groups/{group_id}/members", post(add_members))
        // 读成员列表：P3 命令面（`group.members`）与客户端成员页都要它
        .route("/api/groups/{group_id}/members", get(list_members))
        .route("/api/groups/{group_id}/leave", post(leave))
        .route(
            "/api/groups/{group_id}/members/{user_id}",
            delete(remove_member),
        )
        .route("/api/groups/{group_id}/remark-mine", put(remark_mine))
        .route("/api/groups/{group_id}/visibility", put(set_visibility))
        .route("/api/groups/{group_id}/dissolve", post(dissolve))
}

#[derive(serde::Deserialize)]
pub struct CreateGroup {
    pub name: String,
    #[serde(default)]
    pub member_ids: Vec<String>,
}

#[derive(serde::Deserialize)]
pub struct MemberIds {
    pub user_ids: Vec<String>,
}

/// `GET /api/groups/{id}/members` 的翻页参数（#331 第 3 条，游标下推）。
/// 两个都不给 = 老行为：整份名单。
#[derive(serde::Deserialize)]
pub struct ListMembersQuery {
    pub after_user: Option<String>,
    pub limit: Option<i64>,
}

#[derive(serde::Deserialize)]
pub struct RemarkMine {
    pub remark: String,
}

async fn create_group(
    State(platform): State<PlatformState>,
    authed: Authed,
    headers: axum::http::HeaderMap,
    ApiJson(body): ApiJson<CreateGroup>,
) -> std::result::Result<(StatusCode, Json<serde_json::Value>), ApiError> {
    // agent 会重试：带同一个 Idempotency-Key 时返回第一次的结果，不新建群
    let idem = idempotency_key(&headers);
    // ADR-28 样板 ③「群管理写」：每一次同步 SQL（幂等重放 / 建群 + 成员行 /
    // 审计与幂等回执）都整段进阻塞池，handler 只在两次之间 `await` 扇出。
    let replay_key = idem.clone();
    if let Some(prev) = offload(&platform, move |p| match &replay_key {
        Some(key) => p.replay_idempotent(key),
        None => Ok(None),
    })
    .await?
    {
        return Ok((StatusCode::OK, Json(prev)));
    }

    let owner = authed.user_id.clone();
    let name = body.name.clone();
    let member_ids = body.member_ids.clone();
    let group_name = name.clone();
    let (id, creator_name) = offload(&platform, move |p| {
        let id = p.create_group(&owner, &group_name, &member_ids)?;
        Ok((id, p.profile_of(&owner).map(|x| x.name).unwrap_or_default()))
    })
    .await?;
    let others: Vec<String> = body.member_ids;
    if !others.is_empty() {
        let env = group_notice(
            &id,
            &authed.user_id,
            &others,
            &format!("{creator_name} 创建了群聊「{}」", name),
        );
        platform.emit(env).await;
    }
    let reply = serde_json::json!({ "group_id": id, "members": others.len() + 1 });
    if let Some(key) = idem {
        let stored = reply.clone();
        offload(&platform, move |p| p.remember_idempotent(&key, &stored)).await?;
    }
    Ok((StatusCode::OK, Json(reply)))
}

async fn add_members(
    State(platform): State<PlatformState>,
    authed: Authed,
    headers: axum::http::HeaderMap,
    Path(group_id): Path<String>,
    ApiJson(body): ApiJson<MemberIds>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    // #331 第 2 条：读 `Idempotency-Key` 做重放。加人本身靠主键 +
    // ON CONFLICT 天然幂等，但裸重试第二次拿到的是 `added=0`，调用方
    // 分不清「我上次加过了」和「这人本来就在」——同键重试返回**第一次**
    // 的结果。存储键带操作/群/操作者前缀：`idempotency` 是全域一张表，
    // 裸键会和建群、转账的键撞车。重放在权限校验之前（与建群同形状），
    // 只是把当时的回执再给一遍，不再发通知。
    let idem = idempotency_key(&headers)
        .map(|k| format!("group.add_members:{group_id}:{}:{k}", authed.user_id));
    let replay_key = idem.clone();
    if let Some(prev) = offload(&platform, move |p| match &replay_key {
        Some(key) => p.replay_idempotent(key),
        None => Ok(None),
    })
    .await?
    {
        return Ok(Json(prev));
    }

    // ADR-28 样板 ③「群管理写」：权限 → 人数上限 → 逐行 INSERT → 缓存失效 +
    // 信箱成员快照，全链在一个阻塞任务里跑完（顺序与改前一致）。
    let actor = authed.user_id.clone();
    let user_ids = body.user_ids.clone();
    let conv = group_id.clone();
    let (added, members, name) = offload(&platform, move |p| {
        let added = p.add_members(&conv, &actor, &user_ids)?;
        let members = p.group_members(&conv)?;
        let name = p.profile_of(&actor).map(|m| m.name).unwrap_or_default();
        Ok((added, members, name))
    })
    .await?;
    if added > 0 {
        let recipients: Vec<String> = members
            .iter()
            .map(|m| m.user_id.clone())
            .filter(|u| *u != authed.user_id)
            .collect();
        if !recipients.is_empty() {
            let env = group_notice(
                &group_id,
                &authed.user_id,
                &recipients,
                &format!("{name} 加入了群聊"),
            );
            platform.emit(env).await;
        }
    }
    let reply = serde_json::json!({ "added": added });
    if let Some(key) = idem {
        let stored = reply.clone();
        offload(&platform, move |p| p.remember_idempotent(&key, &stored)).await?;
    }
    Ok(Json(reply))
}

async fn remove_member(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path((group_id, user_id)): Path<(String, String)>,
) -> std::result::Result<StatusCode, ApiError> {
    // ADR-28 样板 ③「群管理写」：删成员行 + 名单读取 + 显示名一次阻塞完成。
    let actor = authed.user_id.clone();
    let conv = group_id.clone();
    let target = user_id.clone();
    let (members, name) = offload(&platform, move |p| {
        p.remove_member(&conv, &actor, &target)?;
        let members = p.group_members(&conv)?;
        Ok((
            members,
            p.profile_of(&actor).map(|x| x.name).unwrap_or_default(),
        ))
    })
    .await?;
    let recipients: Vec<String> = members.iter().map(|m| m.user_id.clone()).collect();
    if !recipients.is_empty() {
        let env = group_notice(
            &group_id,
            &authed.user_id,
            &recipients,
            &format!("{name} 将用户移出群聊"),
        );
        platform.emit(env).await;
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn leave(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(group_id): Path<String>,
) -> std::result::Result<StatusCode, ApiError> {
    // ADR-28 样板 ③「群管理写」：退群 = 校验角色 + 删行 + 缓存失效 + 快照同步。
    let actor = authed.user_id.clone();
    let conv = group_id.clone();
    let (members, name) = offload(&platform, move |p| {
        p.leave_group(&conv, &actor)?;
        let members = p.group_members(&conv)?;
        Ok((
            members,
            p.profile_of(&actor).map(|x| x.name).unwrap_or_default(),
        ))
    })
    .await?;
    let recipients: Vec<String> = members.iter().map(|m| m.user_id.clone()).collect();
    if !recipients.is_empty() {
        let env = group_notice(
            &group_id,
            &authed.user_id,
            &recipients,
            &format!("{name} 退出了群聊"),
        );
        platform.emit(env).await;
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn remark_mine(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(group_id): Path<String>,
    ApiJson(body): ApiJson<RemarkMine>,
) -> std::result::Result<StatusCode, ApiError> {
    // ADR-28 样板 ③「群管理写」：改自己在那一行的备注（单条 UPDATE）。
    let actor = authed.user_id.clone();
    let remark = body.remark.clone();
    offload(&platform, move |p| {
        p.set_my_remark(&group_id, &actor, &remark)
    })
    .await?;
    Ok(StatusCode::OK)
}

#[derive(serde::Deserialize)]
pub struct VisibilityBody {
    pub visibility: String,
}

async fn set_visibility(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(group_id): Path<String>,
    ApiJson(body): ApiJson<VisibilityBody>,
) -> std::result::Result<StatusCode, ApiError> {
    let group_id = validate::id(&group_id).map_err(ApiError::from)?.to_string();
    // ADR-28 样板 ③「群管理写」：归属校验 + 元数据 upsert + 审计写，一次阻塞。
    let actor = authed.user_id.clone();
    let visibility = body.visibility.clone();
    let target = group_id.clone();
    offload(&platform, move |p| {
        p.set_group_visibility(&actor, &target, &visibility)?;
        p.audit(&actor, "group.visibility", &target, &visibility);
        Ok(())
    })
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// 解散群：群主专属。给每个成员发一条 `group.dissolved` 通知帧。
async fn dissolve(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(group_id): Path<String>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let group_id = validate::id(&group_id).map_err(ApiError::from)?.to_string();
    // ADR-28 样板 ③「群管理写」：解散写 + 审计写在同一段阻塞里；逐成员扇出仍
    // 留在 await 侧（那是网络投递，不是 SQL）。
    let actor = authed.user_id.clone();
    let target = group_id.clone();
    let members = offload(&platform, move |p| {
        let members = p.dissolve_group(&actor, &target)?;
        p.audit(&actor, "group.dissolve", &target, "");
        Ok(members)
    })
    .await?;
    for member in &members {
        let mut env =
            crate::system_envelope(&format!("sys-{member}"), "system", vec![member.clone()], "");
        env.body = wxlike_core::MsgBody::Custom {
            kind: "group.dissolved".into(),
            payload: serde_json::json!({
                "group_id": group_id,
                "by": authed.user_id,
                "at_ms": wxlike_core::types::now_ms(),
            }),
        };
        platform.emit(env).await;
    }
    Ok(Json(
        serde_json::json!({ "dissolved": true, "notified": members.len() }),
    ))
}

// 群管理的三个读接口（group_info / list_members / my_groups）没在本卡三条样板热点
// 里，逐个点位下面的 `TODO(ADR-28)` 各自记一笔，改造方式与 `dissolve` 相同。
async fn group_info(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(group_id): Path<String>,
) -> std::result::Result<Json<GroupInfo>, ApiError> {
    // TODO(ADR-28): 一次请求里 4 次上锁（header/role/meta/members）全在 await
    // 上下文里同步跑；改法：整段读包进一次 `offload`。
    let group_id = validate::id(&group_id).map_err(ApiError::from)?.to_string();
    // existence first: "no such group" is a 404, "not yours to read" is a 403
    let (id, name, owner_uid, created_at) = platform
        .group_header(&group_id)?
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "no such group"))?;
    let my_role = platform
        .role_of(&group_id, &authed.user_id)
        .map_err(|_| ApiError::new(StatusCode::FORBIDDEN, "not a member of this group"))?;
    let (visibility, dissolved) = platform.group_meta_of(&group_id)?;
    Ok(Json(GroupInfo {
        id,
        name,
        owner_uid,
        created_at,
        my_role,
        visibility,
        dissolved,
        members: platform.group_members(&group_id)?,
    }))
}

/// 群成员列表。只对群成员开放——非成员拿到 403 而不是空列表，
/// 否则"这个群有多少人"就成了公开信息。
///
/// 带 `after_user` / `limit` 时走 `group_members_page` 的 SQL 下推
/// （#331 第 3 条）；两个参数都不给时与旧行为逐字一致：整份名单。
async fn list_members(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(group_id): Path<String>,
    Query(q): Query<ListMembersQuery>,
) -> std::result::Result<Json<Vec<MemberRow>>, ApiError> {
    // TODO(ADR-28): 成员名单这条读（含 `is_member` 的存在性校验）在 await 上下文
    // 里上锁同步跑；与 `group_info` 同一类点位。
    if !platform.is_member(&group_id, &authed.user_id)? {
        return Err(PlatformError::Forbidden.into());
    }
    if q.after_user.is_none() && q.limit.is_none() {
        return Ok(Json(platform.group_members(&group_id)?));
    }
    Ok(Json(platform.group_members_page(
        &group_id,
        q.after_user.as_deref(),
        q.limit,
    )?))
}

async fn my_groups(
    State(platform): State<PlatformState>,
    authed: Authed,
) -> std::result::Result<Json<Vec<GroupSummary>>, ApiError> {
    // TODO(ADR-28): "我的群"这条带子查询的读在 await 上下文里同步上锁；
    // 与 `group_info` / `list_members` 同一类点位。
    Ok(Json(platform.group_summary(&authed.user_id)?))
}
