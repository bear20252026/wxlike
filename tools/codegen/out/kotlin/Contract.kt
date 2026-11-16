// 由 tools/codegen/generate.py 生成，**不要手改**。
// schema: contract-v1  摘要: 470793a94f99707f  contract: 1.0
// 改了契约就重跑生成器；`--check` 会在 CI 上盯着这一步。


@file:Suppress("unused")

package wxlike.contract

const val SCHEMA_VERSION = "contract-v1"
const val CONTRACT_VERSION = "1.0"
const val SCHEMA_DIGEST = "470793a94f99707f"

data class Money(
    val minor: String,
    val currency: String
)

data class DomainError(
    val code: String,
    val message: String,
    val field: String? = null,
    val retry_after_ms: Long? = null
)

data class ActionRequest(
    val action_id: String,
    val entry_id: String? = null,
    val params: Map<String, Any>,
    val idempotency_key: String? = null,
    val deadline: String? = null,
    val trace_id: String,
    val contract: String
)

data class ActionResult(
    val action_id: String,
    val status: String,
    val data: Map<String, Any>? = null,
    val error: Map<String, Any>? = null,
    val cursor: String? = null,
    val completed_at_ms: Long
)

data class EventEnvelope(
    val event_id: String,
    val seq: String,
    val conv: String? = null,
    val kind: String,
    val payload: Map<String, Any>,
    val emitted_at_ms: Long,
    val contract: String
)

// 已登记的命令（与 crates/cmd 的目录同源）。
val actionCapabilities: Map<String, String> = mapOf(
    "channel.roles" to "read",
    "community.threads" to "read",
    "conv.list" to "read",
    "conv.unread" to "read",
    "conv.history" to "read",
    "conv.sync" to "read",
    "conv.ack" to "write",
    "friend.list" to "read",
    "friend.requests.incoming" to "read",
    "friend.requests.outgoing" to "read",
    "friend.request.create" to "write",
    "friend.request.accept" to "write",
    "friend.request.decline" to "write",
    "friend.remark.set" to "write",
    "friend.remove" to "destructive",
    "friend.block.list" to "read",
    "friend.block.add" to "destructive",
    "friend.block.remove" to "write",
    "friend.privacy.get" to "read",
    "friend.privacy.set" to "write",
    "group.members" to "read",
    "group.info" to "read",
    "group.announcement.get" to "read",
    "group.mute_all.get" to "read",
    "group.create" to "write",
    "group.add_member" to "write",
    "group.announcement.set" to "write",
    "group.mute_all.set" to "write",
    "group.kick" to "destructive",
    "me.info" to "read",
    "me.sessions" to "read",
    "media.files.list" to "read",
    "media.upload.get" to "read",
    "media.upload.start" to "write",
    "media.upload.complete" to "write",
    "media.upload.cancel" to "write",
    "media.object.access" to "write",
    "media.object.revoke" to "write",
    "msg.list" to "read",
    "msg.search" to "read",
    "msg.get" to "read",
    "msg.send" to "write",
    "msg.forward" to "write",
    "msg.recall" to "destructive",
    "video.select" to "read",
    "video.stats" to "read",
    "video.job.get" to "read",
    "video.job.create" to "write",
    "video.job.retry" to "write",
    "video.view.report" to "write",
)

// 一条命令参数的规格（与 crates/cmd 的 Param 声明同源）。
data class ParamSpec(val name: String, val type: String, val required: Boolean, val note: String)

// 逐命令的参数规格。
val actionParams: Map<String, List<ParamSpec>> = mapOf(
    "channel.roles" to listOf(ParamSpec("conv", "String", true, "频道 id")),
    "community.threads" to listOf(ParamSpec("community", "String", true, "社区 id"), ParamSpec("limit", "Long", false, "条数上限")),
    "conv.list" to emptyList(),
    "conv.unread" to listOf(ParamSpec("conv", "String", true, "会话 id")),
    "conv.history" to listOf(ParamSpec("conv", "String", true, "会话 id"), ParamSpec("before_seq", "Long", false, "只取 seq 小于它的"), ParamSpec("limit", "Long", false, "条数上限")),
    "conv.sync" to listOf(ParamSpec("conv", "String", true, "会话 id"), ParamSpec("version", "Long", true, "客户端持有的版本号")),
    "conv.ack" to listOf(ParamSpec("conv", "String", true, "会话 id"), ParamSpec("seq", "Long", false, "推到哪个 seq")),
    "friend.list" to emptyList(),
    "friend.requests.incoming" to emptyList(),
    "friend.requests.outgoing" to emptyList(),
    "friend.request.create" to listOf(ParamSpec("to_user_id", "String", true, "要添加的那个人 user_id"), ParamSpec("message", "String", false, "申请附言；不给=只发空附言，服务端限长")),
    "friend.request.accept" to listOf(ParamSpec("req_id", "String", true, "申请 id（incoming 列表里的 id 字段）")),
    "friend.request.decline" to listOf(ParamSpec("req_id", "String", true, "申请 id（incoming 列表里的 id 字段）")),
    "friend.remark.set" to listOf(ParamSpec("user_id", "String", true, "好友的 user_id"), ParamSpec("remark", "String", true, "备注名（服务端校验长度）")),
    "friend.remove" to listOf(ParamSpec("user_id", "String", true, "要删除的好友 user_id"), ParamSpec("confirmation_token", "String", false, "确认令牌：先 prepare，再把令牌带回来")),
    "friend.block.list" to emptyList(),
    "friend.block.add" to listOf(ParamSpec("user_id", "String", true, "要拉黑的 user_id"), ParamSpec("confirmation_token", "String", false, "确认令牌：先 prepare，再把令牌带回来")),
    "friend.block.remove" to listOf(ParamSpec("user_id", "String", true, "要解除拉黑的 user_id")),
    "friend.privacy.get" to emptyList(),
    "friend.privacy.set" to listOf(ParamSpec("allow", "Boolean", true, "true = 允许陌生人发消息请求（默认）；false = 直接拒绝")),
    "group.members" to listOf(ParamSpec("conv", "String", true, "群 id"), ParamSpec("after_user", "String", false, "游标：上一页最后一个 user_id，从它之后继续；它已不在名单里就给空页"), ParamSpec("limit", "Long", false, "这一页最多几条（缺省=剩下的全给）")),
    "group.info" to listOf(ParamSpec("conv", "String", true, "群 id")),
    "group.announcement.get" to listOf(ParamSpec("conv", "String", true, "群 id")),
    "group.mute_all.get" to listOf(ParamSpec("conv", "String", true, "群 id")),
    "group.create" to listOf(ParamSpec("name", "String", true, "群名"), ParamSpec("member_ids", "List<String>", false, "初始成员 user_id 列表"), ParamSpec("idempotency_key", "String", true, "幂等键")),
    "group.add_member" to listOf(ParamSpec("conv", "String", true, "群 id"), ParamSpec("user_ids", "List<String>", true, "要拉进来的 user_id 列表")),
    "group.announcement.set" to listOf(ParamSpec("conv", "String", true, "群 id"), ParamSpec("text", "String", true, "公告全文（上限 2000 字，服务端校验）"), ParamSpec("media_id", "String", false, "附带媒体 id（可选，服务端默认空）")),
    "group.mute_all.set" to listOf(ParamSpec("conv", "String", true, "群 id"), ParamSpec("on", "Boolean", true, "true = 开启全员禁言；false = 关闭")),
    "group.kick" to listOf(ParamSpec("conv", "String", true, "群 id"), ParamSpec("user_id", "String", true, "被移出的 user_id"), ParamSpec("confirmation_token", "String", false, "确认令牌：先 prepare，再把令牌带回来")),
    "me.info" to emptyList(),
    "me.sessions" to emptyList(),
    "media.files.list" to listOf(ParamSpec("category", "String", false, "分类（如 file/image/video/voice，服务端定义）"), ParamSpec("folder", "String", false, "文件夹（服务端定义）"), ParamSpec("sort", "String", false, "排序：recent（默认）/ name / size"), ParamSpec("cursor", "String", false, "上一页给的续页游标"), ParamSpec("limit", "Long", false, "本页条数（服务端有上限）")),
    "media.upload.get" to listOf(ParamSpec("upload_id", "String", true, "上传会话 id（up_ 前缀）")),
    "media.upload.start" to listOf(ParamSpec("purpose", "String", true, "用途：attachment / avatar / sticker / voice / video / file / backup / derived（服务端校验）"), ParamSpec("name", "String", true, "文件名"), ParamSpec("size", "Long", true, "声明大小（字节）"), ParamSpec("mime", "String", true, "MIME 类型"), ParamSpec("digest", "String", false, "内容 SHA-256（hex）：填了才有秒传快路径，服务端会核对")),
    "media.upload.complete" to listOf(ParamSpec("upload_id", "String", true, "上传会话 id（up_ 前缀）"), ParamSpec("digest", "String", false, "期望的内容 SHA-256（hex，可选，服务端核对）")),
    "media.upload.cancel" to listOf(ParamSpec("upload_id", "String", true, "上传会话 id（up_ 前缀）")),
    "media.object.access" to listOf(ParamSpec("object_id", "String", true, "对象 id（内容 digest）")),
    "media.object.revoke" to listOf(ParamSpec("object_id", "String", true, "对象 id（内容 digest）")),
    "msg.list" to listOf(ParamSpec("conv", "String", true, "会话 id"), ParamSpec("from_seq", "Long", true, "起始 seq（含）"), ParamSpec("to_seq", "Long", false, "结束 seq（含）")),
    "msg.search" to listOf(ParamSpec("q", "String", true, "搜索词"), ParamSpec("limit", "Long", false, "条数上限")),
    "msg.get" to listOf(ParamSpec("conv", "String", true, "会话 id"), ParamSpec("msg_id", "String", true, "消息 id")),
    "msg.send" to listOf(ParamSpec("conv", "String", true, "会话 id"), ParamSpec("text", "String", true, "消息正文"), ParamSpec("reply_to", "String", false, "引用回复：被引消息的 msg_id（hex）；服务端会校验它属于本会话且未撤回"), ParamSpec("idempotency_key", "String", true, "幂等键")),
    "msg.forward" to listOf(ParamSpec("from_conv", "String", true, "来源会话"), ParamSpec("to_conv", "String", true, "目标会话"), ParamSpec("msg_id", "String", true, "消息 id"), ParamSpec("idempotency_key", "String", true, "幂等键")),
    "msg.recall" to listOf(ParamSpec("conv", "String", true, "会话 id"), ParamSpec("msg_id", "String", true, "消息 id"), ParamSpec("confirmation_token", "String", false, "确认令牌：先 prepare，再把令牌带回来")),
    "video.select" to listOf(ParamSpec("video_id", "String", true, "视频 id"), ParamSpec("kbps", "Long", true, "实测可用带宽（千比特/秒）")),
    "video.stats" to listOf(ParamSpec("video_id", "String", true, "视频 id")),
    "video.job.get" to listOf(ParamSpec("job_id", "String", true, "作业 id")),
    "video.job.create" to listOf(ParamSpec("path", "String", true, "本地文件路径"), ParamSpec("callback_url", "String", false, "完成后回调地址")),
    "video.job.retry" to listOf(ParamSpec("job_id", "String", true, "作业 id")),
    "video.view.report" to listOf(ParamSpec("video_id", "String", true, "视频 id"), ParamSpec("viewer", "String", true, "观众 user_id"), ParamSpec("watched_ms", "Long", true, "本次新增观看时长"), ParamSpec("position_ms", "Long", false, "当前播放位置"), ParamSpec("completed", "Boolean", false, "是否看完"), ParamSpec("session_id", "String", false, "播放会话 id")),
)

// 解码一条命令结果；未知 status 返回 null（跳过，不抛）。
//
// 服务端先上线新状态、旧客户端后升级是常态：这时候应当"跳过并记一笔"，
// 不是抛异常把整条渲染链打断。与 TS/Dart 的 decodeActionResult 同语义。
fun decodeActionResult(raw: Any?): ActionResult? {
    val value = raw as? Map<*, *> ?: return null
    val actionId = value["action_id"] as? String ?: return null
    val status = value["status"] as? String ?: return null
    if (status != "ok" && status != "empty" && status != "failed") return null
    val completedAtMs = (value["completed_at_ms"] as? Number)?.toLong() ?: return null
    return ActionResult(
        action_id = actionId,
        status = status,
        data = value["data"] as? Map<String, Any>,
        error = value["error"] as? Map<String, Any>,
        cursor = value["cursor"] as? String,
        completed_at_ms = completedAtMs,
    )
}

// 这条命令认不认识。
fun isKnownAction(actionId: String): Boolean = actionCapabilities.containsKey(actionId)

// FFI 事件帧（S2）：CoreHandle.poll / subscribe 推出的每条消息。
// result 帧携带规范 ActionResult，与 action.schema 逐字段一致。
sealed class CoreFrame {
    data class Result(val requestId: String, val result: ActionResult) : CoreFrame()
    data class Error(val requestId: String, val error: Map<String, Any>, val retryable: Boolean) : CoreFrame()
    data class Fatal(val message: String) : CoreFrame()
}

// 解码一条 FFI 帧；未知 kind 返回 null（跳过，不抛）。
fun decodeCoreFrame(raw: Any?): CoreFrame? {
    val value = raw as? Map<*, *> ?: return null
    return when (value["kind"]) {
        "result" -> {
            val requestId = value["request_id"] as? String ?: return null
            val result = decodeActionResult(value) ?: return null
            CoreFrame.Result(requestId, result)
        }
        "error" -> {
            val requestId = value["request_id"] as? String ?: return null
            val error = value["error"] as? Map<String, Any> ?: return null
            CoreFrame.Error(requestId, error, value["retryable"] == true)
        }
        "fatal" -> {
            val message = value["message"] as? String ?: return null
            CoreFrame.Fatal(message)
        }
        else -> null
    }
}

