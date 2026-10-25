/// 消息端口与事件源。
///
/// Rust 绑定目前只有请求/响应（list/history/send/read/outbox），没有推送订阅。
/// 因此 [FfiMessagingPort.watch] 用前台轮询把"新消息到达"变成事件流；这补齐了
/// 真机验收记录的"聊天屏没有实时接收"缺口。G03-05 的推送上线后，只需替换本类，
/// 控制器与页面不用改。
library;

import 'dart:async';

import '../../../shell/contracts.dart';
import '../../../src/service.dart';

sealed class ChatEvent {
  const ChatEvent();
}

class MessagesUpdated extends ChatEvent {
  const MessagesUpdated(this.messages, {required this.fromCache});

  final List<ChatMessage> messages;
  final bool fromCache;
}

class UnreadUpdated extends ChatEvent {
  const UnreadUpdated(this.unread);

  final UnreadState unread;
}

class OutboxUpdated extends ChatEvent {
  const OutboxUpdated(this.summary);

  final OutboxSummary summary;
}

class ConversationsUpdated extends ChatEvent {
  const ConversationsUpdated(this.list, this.live);

  final ConversationList list;
  final LiveInfo live;
}

class ChatWatchFailed extends ChatEvent {
  const ChatWatchFailed(this.failure);

  final G12Failure failure;
}

abstract class MessagingPort {
  SessionInfo session();

  Future<ConversationList> conversations();

  Future<LiveInfo> liveInfo();

  Future<MessagePage> history(String conv, int beforeSeq, int limit);

  Future<UnreadState> unread(String conv);

  Future<UnreadState> markRead(String conv, int uptoSeq);

  Future<SendOutcome> sendText(String conv, String text);

  Future<OutboxSummary> outboxStatus();

  Future<FlushReport> flushOutbox();

  Future<OutboxSummary> retryOutbox(String taskId);

  Future<bool> dropOutbox(String taskId);

  Future<LiveInfo> connect();

  /// `conv` 为空表示"会话列表"事件。取消订阅即停止轮询。
  Stream<ChatEvent> watch(String conv, {Duration interval});
}

class FfiMessagingPort implements MessagingPort {
  FfiMessagingPort(this._service);

  final WxlikeService _service;

  @override
  SessionInfo session() => _service.session();

  @override
  Future<ConversationList> conversations() => _call(_service.conversations, '加载会话列表');

  @override
  Future<LiveInfo> liveInfo() => _call(_service.liveInfo, '读取在线状态');

  @override
  Future<MessagePage> history(String conv, int beforeSeq, int limit) =>
      _call(() => _service.history(conv, beforeSeq, limit), '加载历史消息');

  @override
  Future<UnreadState> unread(String conv) => _call(() => _service.unread(conv), '读取未读');

  @override
  Future<UnreadState> markRead(String conv, int uptoSeq) =>
      _call(() => _service.markRead(conv, uptoSeq), '标记已读');

  @override
  Future<SendOutcome> sendText(String conv, String text) =>
      _call(() => _service.sendText(conv, text), '发送消息');

  @override
  Future<OutboxSummary> outboxStatus() => _call(_service.outboxStatus, '读取待发队列');

  @override
  Future<FlushReport> flushOutbox() => _call(_service.flushOutbox, '补发队列');

  @override
  Future<OutboxSummary> retryOutbox(String taskId) =>
      _call(() => _service.retryOutbox(taskId), '重试发送');

  @override
  Future<bool> dropOutbox(String taskId) =>
      _call(() => _service.dropOutbox(taskId), '删除待发消息');

  @override
  Future<LiveInfo> connect() => _call(_service.connect, '连接服务端');

  @override
  Stream<ChatEvent> watch(String conv, {Duration interval = const Duration(seconds: 2)}) {
    late StreamController<ChatEvent> controller;
    Timer? timer;
    var inFlight = false;
    var cancelled = false;

    Future<void> tick() async {
      if (inFlight || cancelled) return;
      inFlight = true;
      try {
        if (conv.isEmpty) {
          final list = await _service.conversations();
          final live = await _service.liveInfo();
          if (!cancelled) controller.add(ConversationsUpdated(list, live));
        } else {
          // 只取最新一页：滚动不会把整段历史复制到 Dart。
          final page = await _service.history(conv, 0, pageSize);
          if (!cancelled) controller.add(MessagesUpdated(page.messages, fromCache: page.fromCache));
          final unread = await _service.unread(conv);
          if (!cancelled) controller.add(UnreadUpdated(unread));
          final outbox = await _service.outboxStatus();
          if (!cancelled) controller.add(OutboxUpdated(outbox));
        }
      } catch (error) {
        if (!cancelled) controller.add(ChatWatchFailed(_map(error)));
      } finally {
        inFlight = false;
      }
    }

    controller = StreamController<ChatEvent>(
      onListen: () {
        tick();
        timer = Timer.periodic(interval, (_) => tick());
      },
      onCancel: () {
        cancelled = true;
        timer?.cancel();
      },
    );
    return controller.stream;
  }

  Future<T> _call<T>(Future<T> Function() run, String operation) async {
    try {
      return await run();
    } catch (error) {
      throw _map(error, operation: operation);
    }
  }

  G12Failure _map(Object error, {String? operation}) {
    if (error is G12Failure) return error;
    if (error is WxlikeErrorExceptionNetwork) {
      return G12Failure(G12FailureKind.offline, 'NETWORK', error.detail, retryable: true);
    }
    if (error is WxlikeErrorExceptionUnauthorized) {
      return G12Failure(G12FailureKind.unauthorized, 'UNAUTHORIZED', error.detail);
    }
    if (error is WxlikeErrorExceptionPermission) {
      return G12Failure(G12FailureKind.forbidden, 'FORBIDDEN', error.detail);
    }
    if (error is WxlikeErrorExceptionUnexpected) {
      return G12Failure(
        G12FailureKind.unexpected,
        'UNEXPECTED',
        operation == null ? error.detail : '$operation 失败：${error.detail}',
        retryable: true,
      );
    }
    return G12Failure(G12FailureKind.unexpected, 'UNEXPECTED', '$error', retryable: true);
  }
}
