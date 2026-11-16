// 由 tools/codegen/generate.py 生成，**不要手改**。
// schema: contract-v1  摘要: 470793a94f99707f  contract: 1.0
// 改了契约就重跑生成器；`--check` 会在 CI 上盯着这一步。


// ignore_for_file: unused_field

const String schemaVersion = 'contract-v1';
const String contractVersion = '1.0';
const String schemaDigest = '470793a94f99707f';

class Money {
  final String minor;
  final String currency;

  const Money({
    this.minor,
    this.currency,
  });

  factory Money.fromJson(Map<String, dynamic> json) {
    return Money(
      minor: json['minor'] as String?,
      currency: json['currency'] as String?,
    );
  }

  Map<String, dynamic> toJson() => {
    'minor': minor,
    'currency': currency,
  };
}

class DomainError {
  final String code;
  final String message;
  final String? field;
  final int? retry_after_ms;

  const DomainError({
    this.code,
    this.message,
    this.field,
    this.retry_after_ms,
  });

  factory DomainError.fromJson(Map<String, dynamic> json) {
    return DomainError(
      code: json['code'] as String?,
      message: json['message'] as String?,
      field: json['field'] as String?,
      retry_after_ms: json['retry_after_ms'] as int?,
    );
  }

  Map<String, dynamic> toJson() => {
    'code': code,
    'message': message,
    'field': field,
    'retry_after_ms': retry_after_ms,
  };
}

class ActionRequest {
  final String action_id;
  final String? entry_id;
  final Map<String, dynamic> params;
  final String? idempotency_key;
  final String? deadline;
  final String trace_id;
  final String contract;

  const ActionRequest({
    this.action_id,
    this.entry_id,
    this.params,
    this.idempotency_key,
    this.deadline,
    this.trace_id,
    this.contract,
  });

  factory ActionRequest.fromJson(Map<String, dynamic> json) {
    return ActionRequest(
      action_id: json['action_id'] as String?,
      entry_id: json['entry_id'] as String?,
      params: json['params'] as Map<String, dynamic>?,
      idempotency_key: json['idempotency_key'] as String?,
      deadline: json['deadline'] as String?,
      trace_id: json['trace_id'] as String?,
      contract: json['contract'] as String?,
    );
  }

  Map<String, dynamic> toJson() => {
    'action_id': action_id,
    'entry_id': entry_id,
    'params': params,
    'idempotency_key': idempotency_key,
    'deadline': deadline,
    'trace_id': trace_id,
    'contract': contract,
  };
}

class ActionResult {
  final String action_id;
  final String status;
  final Map<String, dynamic>? data;
  final Map<String, dynamic>? error;
  final String? cursor;
  final int completed_at_ms;

  const ActionResult({
    this.action_id,
    this.status,
    this.data,
    this.error,
    this.cursor,
    this.completed_at_ms,
  });

  factory ActionResult.fromJson(Map<String, dynamic> json) {
    return ActionResult(
      action_id: json['action_id'] as String?,
      status: json['status'] as String?,
      data: json['data'] as Map<String, dynamic>?,
      error: json['error'] as Map<String, dynamic>?,
      cursor: json['cursor'] as String?,
      completed_at_ms: json['completed_at_ms'] as int?,
    );
  }

  Map<String, dynamic> toJson() => {
    'action_id': action_id,
    'status': status,
    'data': data,
    'error': error,
    'cursor': cursor,
    'completed_at_ms': completed_at_ms,
  };
}

class EventEnvelope {
  final String event_id;
  final String seq;
  final String? conv;
  final String kind;
  final Map<String, dynamic> payload;
  final int emitted_at_ms;
  final String contract;

  const EventEnvelope({
    this.event_id,
    this.seq,
    this.conv,
    this.kind,
    this.payload,
    this.emitted_at_ms,
    this.contract,
  });

  factory EventEnvelope.fromJson(Map<String, dynamic> json) {
    return EventEnvelope(
      event_id: json['event_id'] as String?,
      seq: json['seq'] as String?,
      conv: json['conv'] as String?,
      kind: json['kind'] as String?,
      payload: json['payload'] as Map<String, dynamic>?,
      emitted_at_ms: json['emitted_at_ms'] as int?,
      contract: json['contract'] as String?,
    );
  }

  Map<String, dynamic> toJson() => {
    'event_id': event_id,
    'seq': seq,
    'conv': conv,
    'kind': kind,
    'payload': payload,
    'emitted_at_ms': emitted_at_ms,
    'contract': contract,
  };
}

/// 已登记的命令（与 crates/cmd 的目录同源）。
const Map<String, String> actionCapabilities = {
  'channel.roles': 'read',
  'community.threads': 'read',
  'conv.list': 'read',
  'conv.unread': 'read',
  'conv.history': 'read',
  'conv.sync': 'read',
  'conv.ack': 'write',
  'friend.list': 'read',
  'friend.requests.incoming': 'read',
  'friend.requests.outgoing': 'read',
  'friend.request.create': 'write',
  'friend.request.accept': 'write',
  'friend.request.decline': 'write',
  'friend.remark.set': 'write',
  'friend.remove': 'destructive',
  'friend.block.list': 'read',
  'friend.block.add': 'destructive',
  'friend.block.remove': 'write',
  'friend.privacy.get': 'read',
  'friend.privacy.set': 'write',
  'group.members': 'read',
  'group.info': 'read',
  'group.announcement.get': 'read',
  'group.mute_all.get': 'read',
  'group.create': 'write',
  'group.add_member': 'write',
  'group.announcement.set': 'write',
  'group.mute_all.set': 'write',
  'group.kick': 'destructive',
  'me.info': 'read',
  'me.sessions': 'read',
  'media.files.list': 'read',
  'media.upload.get': 'read',
  'media.upload.start': 'write',
  'media.upload.complete': 'write',
  'media.upload.cancel': 'write',
  'media.object.access': 'write',
  'media.object.revoke': 'write',
  'msg.list': 'read',
  'msg.search': 'read',
  'msg.get': 'read',
  'msg.send': 'write',
  'msg.forward': 'write',
  'msg.recall': 'destructive',
  'video.select': 'read',
  'video.stats': 'read',
  'video.job.get': 'read',
  'video.job.create': 'write',
  'video.job.retry': 'write',
  'video.view.report': 'write',
};

/// 一条命令参数的规格（与 crates/cmd 的 Param 声明同源）。
class ParamSpec {
  final String name;
  final String type;
  final bool required;
  final String note;
  const ParamSpec(this.name, this.type, this.required, this.note);
}

/// 逐命令的参数规格。
const Map<String, List<ParamSpec>> actionParams = {
  'channel.roles': [ParamSpec('conv', 'String', true, "频道 id")],
  'community.threads': [ParamSpec('community', 'String', true, "社区 id"), ParamSpec('limit', 'int', false, "条数上限")],
  'conv.list': [],
  'conv.unread': [ParamSpec('conv', 'String', true, "会话 id")],
  'conv.history': [ParamSpec('conv', 'String', true, "会话 id"), ParamSpec('before_seq', 'int', false, "只取 seq 小于它的"), ParamSpec('limit', 'int', false, "条数上限")],
  'conv.sync': [ParamSpec('conv', 'String', true, "会话 id"), ParamSpec('version', 'int', true, "客户端持有的版本号")],
  'conv.ack': [ParamSpec('conv', 'String', true, "会话 id"), ParamSpec('seq', 'int', false, "推到哪个 seq")],
  'friend.list': [],
  'friend.requests.incoming': [],
  'friend.requests.outgoing': [],
  'friend.request.create': [ParamSpec('to_user_id', 'String', true, "要添加的那个人 user_id"), ParamSpec('message', 'String', false, "申请附言；不给=只发空附言，服务端限长")],
  'friend.request.accept': [ParamSpec('req_id', 'String', true, "申请 id（incoming 列表里的 id 字段）")],
  'friend.request.decline': [ParamSpec('req_id', 'String', true, "申请 id（incoming 列表里的 id 字段）")],
  'friend.remark.set': [ParamSpec('user_id', 'String', true, "好友的 user_id"), ParamSpec('remark', 'String', true, "备注名（服务端校验长度）")],
  'friend.remove': [ParamSpec('user_id', 'String', true, "要删除的好友 user_id"), ParamSpec('confirmation_token', 'String', false, "确认令牌：先 prepare，再把令牌带回来")],
  'friend.block.list': [],
  'friend.block.add': [ParamSpec('user_id', 'String', true, "要拉黑的 user_id"), ParamSpec('confirmation_token', 'String', false, "确认令牌：先 prepare，再把令牌带回来")],
  'friend.block.remove': [ParamSpec('user_id', 'String', true, "要解除拉黑的 user_id")],
  'friend.privacy.get': [],
  'friend.privacy.set': [ParamSpec('allow', 'bool', true, "true = 允许陌生人发消息请求（默认）；false = 直接拒绝")],
  'group.members': [ParamSpec('conv', 'String', true, "群 id"), ParamSpec('after_user', 'String', false, "游标：上一页最后一个 user_id，从它之后继续；它已不在名单里就给空页"), ParamSpec('limit', 'int', false, "这一页最多几条（缺省=剩下的全给）")],
  'group.info': [ParamSpec('conv', 'String', true, "群 id")],
  'group.announcement.get': [ParamSpec('conv', 'String', true, "群 id")],
  'group.mute_all.get': [ParamSpec('conv', 'String', true, "群 id")],
  'group.create': [ParamSpec('name', 'String', true, "群名"), ParamSpec('member_ids', 'List<String>', false, "初始成员 user_id 列表"), ParamSpec('idempotency_key', 'String', true, "幂等键")],
  'group.add_member': [ParamSpec('conv', 'String', true, "群 id"), ParamSpec('user_ids', 'List<String>', true, "要拉进来的 user_id 列表")],
  'group.announcement.set': [ParamSpec('conv', 'String', true, "群 id"), ParamSpec('text', 'String', true, "公告全文（上限 2000 字，服务端校验）"), ParamSpec('media_id', 'String', false, "附带媒体 id（可选，服务端默认空）")],
  'group.mute_all.set': [ParamSpec('conv', 'String', true, "群 id"), ParamSpec('on', 'bool', true, "true = 开启全员禁言；false = 关闭")],
  'group.kick': [ParamSpec('conv', 'String', true, "群 id"), ParamSpec('user_id', 'String', true, "被移出的 user_id"), ParamSpec('confirmation_token', 'String', false, "确认令牌：先 prepare，再把令牌带回来")],
  'me.info': [],
  'me.sessions': [],
  'media.files.list': [ParamSpec('category', 'String', false, "分类（如 file/image/video/voice，服务端定义）"), ParamSpec('folder', 'String', false, "文件夹（服务端定义）"), ParamSpec('sort', 'String', false, "排序：recent（默认）/ name / size"), ParamSpec('cursor', 'String', false, "上一页给的续页游标"), ParamSpec('limit', 'int', false, "本页条数（服务端有上限）")],
  'media.upload.get': [ParamSpec('upload_id', 'String', true, "上传会话 id（up_ 前缀）")],
  'media.upload.start': [ParamSpec('purpose', 'String', true, "用途：attachment / avatar / sticker / voice / video / file / backup / derived（服务端校验）"), ParamSpec('name', 'String', true, "文件名"), ParamSpec('size', 'int', true, "声明大小（字节）"), ParamSpec('mime', 'String', true, "MIME 类型"), ParamSpec('digest', 'String', false, "内容 SHA-256（hex）：填了才有秒传快路径，服务端会核对")],
  'media.upload.complete': [ParamSpec('upload_id', 'String', true, "上传会话 id（up_ 前缀）"), ParamSpec('digest', 'String', false, "期望的内容 SHA-256（hex，可选，服务端核对）")],
  'media.upload.cancel': [ParamSpec('upload_id', 'String', true, "上传会话 id（up_ 前缀）")],
  'media.object.access': [ParamSpec('object_id', 'String', true, "对象 id（内容 digest）")],
  'media.object.revoke': [ParamSpec('object_id', 'String', true, "对象 id（内容 digest）")],
  'msg.list': [ParamSpec('conv', 'String', true, "会话 id"), ParamSpec('from_seq', 'int', true, "起始 seq（含）"), ParamSpec('to_seq', 'int', false, "结束 seq（含）")],
  'msg.search': [ParamSpec('q', 'String', true, "搜索词"), ParamSpec('limit', 'int', false, "条数上限")],
  'msg.get': [ParamSpec('conv', 'String', true, "会话 id"), ParamSpec('msg_id', 'String', true, "消息 id")],
  'msg.send': [ParamSpec('conv', 'String', true, "会话 id"), ParamSpec('text', 'String', true, "消息正文"), ParamSpec('reply_to', 'String', false, "引用回复：被引消息的 msg_id（hex）；服务端会校验它属于本会话且未撤回"), ParamSpec('idempotency_key', 'String', true, "幂等键")],
  'msg.forward': [ParamSpec('from_conv', 'String', true, "来源会话"), ParamSpec('to_conv', 'String', true, "目标会话"), ParamSpec('msg_id', 'String', true, "消息 id"), ParamSpec('idempotency_key', 'String', true, "幂等键")],
  'msg.recall': [ParamSpec('conv', 'String', true, "会话 id"), ParamSpec('msg_id', 'String', true, "消息 id"), ParamSpec('confirmation_token', 'String', false, "确认令牌：先 prepare，再把令牌带回来")],
  'video.select': [ParamSpec('video_id', 'String', true, "视频 id"), ParamSpec('kbps', 'int', true, "实测可用带宽（千比特/秒）")],
  'video.stats': [ParamSpec('video_id', 'String', true, "视频 id")],
  'video.job.get': [ParamSpec('job_id', 'String', true, "作业 id")],
  'video.job.create': [ParamSpec('path', 'String', true, "本地文件路径"), ParamSpec('callback_url', 'String', false, "完成后回调地址")],
  'video.job.retry': [ParamSpec('job_id', 'String', true, "作业 id")],
  'video.view.report': [ParamSpec('video_id', 'String', true, "视频 id"), ParamSpec('viewer', 'String', true, "观众 user_id"), ParamSpec('watched_ms', 'int', true, "本次新增观看时长"), ParamSpec('position_ms', 'int', false, "当前播放位置"), ParamSpec('completed', 'bool', false, "是否看完"), ParamSpec('session_id', 'String', false, "播放会话 id")],
};

/// 解码一条命令结果；未知 status 返回 null（跳过，不抛）。
ActionResult? decodeActionResult(Object? raw) {
  if (raw is! Map<String, dynamic>) return null;
  final status = raw['status'];
  if (status != 'ok' && status != 'empty' && status != 'failed') return null;
  if (raw['action_id'] is! String) return null;
  return ActionResult.fromJson(raw);
}

/// 金额：字符串 → BigInt（Dart web 上 int 是 double，必须走 BigInt）。
BigInt moneyToMinor(Money money) => BigInt.parse(money.minor!);

/// 这条命令认不认识。
bool isKnownAction(String actionId) => actionCapabilities.containsKey(actionId);

/// 解码一条 FFI 帧（S2）；未知 kind 返回 null（跳过，不抛）。
/// result 帧把规范 ActionResult 整个 flatten，decodeActionResult 可直接解。
/// 返回规范化 Map：result 帧含 kind/request_id + ActionResult 字段。
Map<String, dynamic>? decodeCoreFrame(Object? raw) {
  if (raw is! Map<String, dynamic>) return null;
  final kind = raw['kind'];
  if (kind == 'result') {
    final result = decodeActionResult(raw);
    if (result == null || raw['request_id'] is! String) return null;
    return {'kind': 'result', 'request_id': raw['request_id'], ...result.toJson()};
  }
  if (kind == 'error') {
    if (raw['request_id'] is! String || raw['error'] is! Map<String, dynamic>) return null;
    return {'kind': 'error', 'request_id': raw['request_id'], 'error': raw['error'], 'retryable': raw['retryable'] == true};
  }
  if (kind == 'fatal') {
    if (raw['message'] is! String) return null;
    return {'kind': 'fatal', 'message': raw['message']};
  }
  return null;
}

