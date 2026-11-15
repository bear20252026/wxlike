// 由 tools/codegen/generate.py 生成，**不要手改**。
// schema: contract-v1  摘要: ad2d2be657637cf7  contract: 1.0
// 改了契约就重跑生成器；`--check` 会在 CI 上盯着这一步。


import Foundation

public let schemaVersion = "contract-v1"
public let contractVersion = "1.0"
public let schemaDigest = "ad2d2be657637cf7"

public struct Money {
    public let minor: String
    public let currency: String

    public init(minor: String, currency: String) {
        self.minor = minor
        self.currency = currency
    }
}

public struct DomainError {
    public let code: String
    public let message: String
    public let field: String?
    public let retry_after_ms: Int?

    public init(code: String, message: String, field: String?, retry_after_ms: Int?) {
        self.code = code
        self.message = message
        self.field = field
        self.retry_after_ms = retry_after_ms
    }
}

public struct ActionRequest {
    public let action_id: String
    public let entry_id: String?
    public let params: [String: Any]
    public let idempotency_key: String?
    public let deadline: String?
    public let trace_id: String
    public let contract: String

    public init(action_id: String, entry_id: String?, params: [String: Any], idempotency_key: String?, deadline: String?, trace_id: String, contract: String) {
        self.action_id = action_id
        self.entry_id = entry_id
        self.params = params
        self.idempotency_key = idempotency_key
        self.deadline = deadline
        self.trace_id = trace_id
        self.contract = contract
    }
}

public struct ActionResult {
    public let action_id: String
    public let status: String
    public let data: [String: Any]?
    public let error: [String: Any]?
    public let cursor: String?
    public let completed_at_ms: Int

    public init(action_id: String, status: String, data: [String: Any]?, error: [String: Any]?, cursor: String?, completed_at_ms: Int) {
        self.action_id = action_id
        self.status = status
        self.data = data
        self.error = error
        self.cursor = cursor
        self.completed_at_ms = completed_at_ms
    }
}

public struct EventEnvelope {
    public let event_id: String
    public let seq: String
    public let conv: String?
    public let kind: String
    public let payload: [String: Any]
    public let emitted_at_ms: Int
    public let contract: String

    public init(event_id: String, seq: String, conv: String?, kind: String, payload: [String: Any], emitted_at_ms: Int, contract: String) {
        self.event_id = event_id
        self.seq = seq
        self.conv = conv
        self.kind = kind
        self.payload = payload
        self.emitted_at_ms = emitted_at_ms
        self.contract = contract
    }
}

/// 已登记的命令（与 crates/cmd 的目录同源）。
public let actionCapabilities: [String: String] = [
    "channel.roles": "read",
    "community.threads": "read",
    "conv.list": "read",
    "conv.unread": "read",
    "conv.history": "read",
    "conv.sync": "read",
    "conv.ack": "write",
    "friend.list": "read",
    "friend.requests.incoming": "read",
    "friend.requests.outgoing": "read",
    "friend.request.create": "write",
    "friend.request.accept": "write",
    "friend.request.decline": "write",
    "friend.remark.set": "write",
    "friend.remove": "destructive",
    "friend.block.list": "read",
    "friend.block.add": "destructive",
    "friend.block.remove": "write",
    "friend.privacy.get": "read",
    "friend.privacy.set": "write",
    "group.members": "read",
    "group.info": "read",
    "group.announcement.get": "read",
    "group.mute_all.get": "read",
    "group.create": "write",
    "group.add_member": "write",
    "group.announcement.set": "write",
    "group.mute_all.set": "write",
    "group.kick": "destructive",
    "me.info": "read",
    "me.sessions": "read",
    "media.files.list": "read",
    "media.upload.get": "read",
    "media.upload.start": "write",
    "media.upload.complete": "write",
    "media.upload.cancel": "write",
    "media.object.access": "write",
    "media.object.revoke": "write",
    "msg.list": "read",
    "msg.search": "read",
    "msg.get": "read",
    "msg.send": "write",
    "msg.forward": "write",
    "msg.recall": "destructive",
    "video.select": "read",
    "video.stats": "read",
    "video.job.get": "read",
    "video.job.create": "write",
    "video.job.retry": "write",
    "video.view.report": "write",
]

/// 一条命令参数的规格（与 crates/cmd 的 Param 声明同源）。
public struct ParamSpec {
    public let name: String
    public let type: String
    public let required: Bool
    public let note: String
    public init(name: String, type: String, required: Bool, note: String) {
        self.name = name
        self.type = type
        self.required = required
        self.note = note
    }
}

/// 逐命令的参数规格。
public let actionParams: [String: [ParamSpec]] = [
    "channel.roles": [ParamSpec(name: "conv", type: "String", required: true, note: "频道 id")],
    "community.threads": [ParamSpec(name: "community", type: "String", required: true, note: "社区 id"), ParamSpec(name: "limit", type: "Int", required: false, note: "条数上限")],
    "conv.list": [],
    "conv.unread": [ParamSpec(name: "conv", type: "String", required: true, note: "会话 id")],
    "conv.history": [ParamSpec(name: "conv", type: "String", required: true, note: "会话 id"), ParamSpec(name: "before_seq", type: "Int", required: false, note: "只取 seq 小于它的"), ParamSpec(name: "limit", type: "Int", required: false, note: "条数上限")],
    "conv.sync": [ParamSpec(name: "conv", type: "String", required: true, note: "会话 id"), ParamSpec(name: "version", type: "Int", required: true, note: "客户端持有的版本号")],
    "conv.ack": [ParamSpec(name: "conv", type: "String", required: true, note: "会话 id"), ParamSpec(name: "seq", type: "Int", required: false, note: "推到哪个 seq")],
    "friend.list": [],
    "friend.requests.incoming": [],
    "friend.requests.outgoing": [],
    "friend.request.create": [ParamSpec(name: "to_user_id", type: "String", required: true, note: "要添加的那个人 user_id"), ParamSpec(name: "message", type: "String", required: false, note: "申请附言；不给=只发空附言，服务端限长")],
    "friend.request.accept": [ParamSpec(name: "req_id", type: "String", required: true, note: "申请 id（incoming 列表里的 id 字段）")],
    "friend.request.decline": [ParamSpec(name: "req_id", type: "String", required: true, note: "申请 id（incoming 列表里的 id 字段）")],
    "friend.remark.set": [ParamSpec(name: "user_id", type: "String", required: true, note: "好友的 user_id"), ParamSpec(name: "remark", type: "String", required: true, note: "备注名（服务端校验长度）")],
    "friend.remove": [ParamSpec(name: "user_id", type: "String", required: true, note: "要删除的好友 user_id"), ParamSpec(name: "confirmation_token", type: "String", required: false, note: "确认令牌：先 prepare，再把令牌带回来")],
    "friend.block.list": [],
    "friend.block.add": [ParamSpec(name: "user_id", type: "String", required: true, note: "要拉黑的 user_id"), ParamSpec(name: "confirmation_token", type: "String", required: false, note: "确认令牌：先 prepare，再把令牌带回来")],
    "friend.block.remove": [ParamSpec(name: "user_id", type: "String", required: true, note: "要解除拉黑的 user_id")],
    "friend.privacy.get": [],
    "friend.privacy.set": [ParamSpec(name: "allow", type: "Bool", required: true, note: "true = 允许陌生人发消息请求（默认）；false = 直接拒绝")],
    "group.members": [ParamSpec(name: "conv", type: "String", required: true, note: "群 id"), ParamSpec(name: "after_user", type: "String", required: false, note: "游标：上一页最后一个 user_id，从它之后继续；它已不在名单里就给空页"), ParamSpec(name: "limit", type: "Int", required: false, note: "这一页最多几条（缺省=剩下的全给）")],
    "group.info": [ParamSpec(name: "conv", type: "String", required: true, note: "群 id")],
    "group.announcement.get": [ParamSpec(name: "conv", type: "String", required: true, note: "群 id")],
    "group.mute_all.get": [ParamSpec(name: "conv", type: "String", required: true, note: "群 id")],
    "group.create": [ParamSpec(name: "name", type: "String", required: true, note: "群名"), ParamSpec(name: "member_ids", type: "[String]", required: false, note: "初始成员 user_id 列表"), ParamSpec(name: "idempotency_key", type: "String", required: true, note: "幂等键")],
    "group.add_member": [ParamSpec(name: "conv", type: "String", required: true, note: "群 id"), ParamSpec(name: "user_ids", type: "[String]", required: true, note: "要拉进来的 user_id 列表")],
    "group.announcement.set": [ParamSpec(name: "conv", type: "String", required: true, note: "群 id"), ParamSpec(name: "text", type: "String", required: true, note: "公告全文（上限 2000 字，服务端校验）"), ParamSpec(name: "media_id", type: "String", required: false, note: "附带媒体 id（可选，服务端默认空）")],
    "group.mute_all.set": [ParamSpec(name: "conv", type: "String", required: true, note: "群 id"), ParamSpec(name: "on", type: "Bool", required: true, note: "true = 开启全员禁言；false = 关闭")],
    "group.kick": [ParamSpec(name: "conv", type: "String", required: true, note: "群 id"), ParamSpec(name: "user_id", type: "String", required: true, note: "被移出的 user_id"), ParamSpec(name: "confirmation_token", type: "String", required: false, note: "确认令牌：先 prepare，再把令牌带回来")],
    "me.info": [],
    "me.sessions": [],
    "media.files.list": [ParamSpec(name: "category", type: "String", required: false, note: "分类（如 file/image/video/voice，服务端定义）"), ParamSpec(name: "folder", type: "String", required: false, note: "文件夹（服务端定义）"), ParamSpec(name: "sort", type: "String", required: false, note: "排序：recent（默认）/ name / size"), ParamSpec(name: "cursor", type: "String", required: false, note: "上一页给的续页游标"), ParamSpec(name: "limit", type: "Int", required: false, note: "本页条数（服务端有上限）")],
    "media.upload.get": [ParamSpec(name: "upload_id", type: "String", required: true, note: "上传会话 id（up_ 前缀）")],
    "media.upload.start": [ParamSpec(name: "purpose", type: "String", required: true, note: "用途：attachment / avatar / sticker / voice / video / file / backup / derived（服务端校验）"), ParamSpec(name: "name", type: "String", required: true, note: "文件名"), ParamSpec(name: "size", type: "Int", required: true, note: "声明大小（字节）"), ParamSpec(name: "mime", type: "String", required: true, note: "MIME 类型"), ParamSpec(name: "digest", type: "String", required: false, note: "内容 SHA-256（hex）：填了才有秒传快路径，服务端会核对")],
    "media.upload.complete": [ParamSpec(name: "upload_id", type: "String", required: true, note: "上传会话 id（up_ 前缀）"), ParamSpec(name: "digest", type: "String", required: false, note: "期望的内容 SHA-256（hex，可选，服务端核对）")],
    "media.upload.cancel": [ParamSpec(name: "upload_id", type: "String", required: true, note: "上传会话 id（up_ 前缀）")],
    "media.object.access": [ParamSpec(name: "object_id", type: "String", required: true, note: "对象 id（内容 digest）")],
    "media.object.revoke": [ParamSpec(name: "object_id", type: "String", required: true, note: "对象 id（内容 digest）")],
    "msg.list": [ParamSpec(name: "conv", type: "String", required: true, note: "会话 id"), ParamSpec(name: "from_seq", type: "Int", required: true, note: "起始 seq（含）"), ParamSpec(name: "to_seq", type: "Int", required: false, note: "结束 seq（含）")],
    "msg.search": [ParamSpec(name: "q", type: "String", required: true, note: "搜索词"), ParamSpec(name: "limit", type: "Int", required: false, note: "条数上限")],
    "msg.get": [ParamSpec(name: "conv", type: "String", required: true, note: "会话 id"), ParamSpec(name: "msg_id", type: "String", required: true, note: "消息 id")],
    "msg.send": [ParamSpec(name: "conv", type: "String", required: true, note: "会话 id"), ParamSpec(name: "text", type: "String", required: true, note: "消息正文"), ParamSpec(name: "reply_to", type: "String", required: false, note: "引用回复：被引消息的 msg_id（hex）；服务端会校验它属于本会话且未撤回"), ParamSpec(name: "idempotency_key", type: "String", required: true, note: "幂等键")],
    "msg.forward": [ParamSpec(name: "from_conv", type: "String", required: true, note: "来源会话"), ParamSpec(name: "to_conv", type: "String", required: true, note: "目标会话"), ParamSpec(name: "msg_id", type: "String", required: true, note: "消息 id"), ParamSpec(name: "idempotency_key", type: "String", required: true, note: "幂等键")],
    "msg.recall": [ParamSpec(name: "conv", type: "String", required: true, note: "会话 id"), ParamSpec(name: "msg_id", type: "String", required: true, note: "消息 id"), ParamSpec(name: "confirmation_token", type: "String", required: false, note: "确认令牌：先 prepare，再把令牌带回来")],
    "video.select": [ParamSpec(name: "video_id", type: "String", required: true, note: "视频 id"), ParamSpec(name: "kbps", type: "Int", required: true, note: "实测可用带宽（千比特/秒）")],
    "video.stats": [ParamSpec(name: "video_id", type: "String", required: true, note: "视频 id")],
    "video.job.get": [ParamSpec(name: "job_id", type: "String", required: true, note: "作业 id")],
    "video.job.create": [ParamSpec(name: "path", type: "String", required: true, note: "本地文件路径"), ParamSpec(name: "callback_url", type: "String", required: false, note: "完成后回调地址")],
    "video.job.retry": [ParamSpec(name: "job_id", type: "String", required: true, note: "作业 id")],
    "video.view.report": [ParamSpec(name: "video_id", type: "String", required: true, note: "视频 id"), ParamSpec(name: "viewer", type: "String", required: true, note: "观众 user_id"), ParamSpec(name: "watched_ms", type: "Int", required: true, note: "本次新增观看时长"), ParamSpec(name: "position_ms", type: "Int", required: false, note: "当前播放位置"), ParamSpec(name: "completed", type: "Bool", required: false, note: "是否看完"), ParamSpec(name: "session_id", type: "String", required: false, note: "播放会话 id")],
]

/// 解码一条命令结果；未知 status 返回 nil（跳过，不抛）。
///
/// 服务端先上线新状态、旧客户端后升级是常态：这时候应当"跳过并记一笔"，
/// 不是抛异常把整条渲染链打断。与 TS/Dart 的 decodeActionResult 同语义。
public func decodeActionResult(_ raw: Any?) -> ActionResult? {
    guard let value = raw as? [String: Any] else { return nil }
    guard let actionId = value["action_id"] as? String else { return nil }
    guard let status = value["status"] as? String,
          status == "ok" || status == "empty" || status == "failed" else { return nil }
    guard let completedAtMs = value["completed_at_ms"] as? Int else { return nil }
    return ActionResult(
        action_id: actionId,
        status: status,
        data: value["data"] as? [String: Any],
        error: value["error"] as? [String: Any],
        cursor: value["cursor"] as? String,
        completed_at_ms: completedAtMs
    )
}

/// 这条命令认不认识。
public func isKnownAction(_ actionId: String) -> Bool {
    return actionCapabilities[actionId] != nil
}

