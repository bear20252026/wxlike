/// 聊天时间线的投影与实时刷新。
///
/// 关键约束：
/// - 消息按 `msg_id` 去重，按 `seq` 排序；轮询/重连只合并增量。
/// - 内存里最多保留 [maxMessages] 条，滚动不把整段历史复制进 Dart。
/// - `retry` 之后重新查询真实历史，而不是只把 `_pending` 从界面抹掉。
/// - 前台恢复、网络恢复都触发 `flushOutbox` 并重新查询。
library;

import 'dart:async';

import 'package:flutter/foundation.dart';

import '../../../shell/contracts.dart';
import '../../../src/service.dart';
import 'messaging_ports.dart';

enum ChatSignal { resumed, networkRegained }

class TimelineController extends ChangeNotifier {
  TimelineController({
    required this.port,
    required this.conv,
    this.maxMessages = 400,
    this.watchInterval = const Duration(seconds: 2),
  });

  final MessagingPort port;
  final String conv;
  final int maxMessages;
  final Duration watchInterval;

  List<ChatMessage> messages = const [];
  List<OutboxEntry> pending = const [];
  final Set<String> retrying = {};

  int? nextBeforeSeq;
  int? firstUnreadSeq;
  bool fromCache = false;
  bool busy = false;
  bool loadingOlder = false;
  bool sending = false;
  bool initialized = false;
  G12Failure? error;

  StreamSubscription<ChatEvent>? _sub;
  bool _foreground = true;
  bool _disposed = false;
  int _latestSeq = 0;

  bool get canLoadOlder => (nextBeforeSeq ?? -1) > 0 && messages.length < maxMessages && !loadingOlder;

  Future<void> init() async {
    if (busy) return;
    busy = true;
    error = null;
    _safeNotify();
    try {
      UnreadState? unread;
      try {
        unread = await port.unread(conv);
      } catch (_) {
        // 未读不可用不应挡住历史显示；未读失败不伪造 0。
      }
      if (unread != null && unread.unread > 0) {
        final first = unread.headSeq - unread.unread + 1;
        if (first > 0) firstUnreadSeq = first;
      }
      final page = await port.history(conv, 0, pageSize);
      final outbox = await port.outboxStatus();
      if (_disposed) return;
      _mergeMessages(page.messages);
      nextBeforeSeq = page.nextBeforeSeq;
      fromCache = page.fromCache;
      _applyOutbox(outbox);
      initialized = true;
      error = null;
      try {
        await port.markRead(conv, 0);
      } catch (markError) {
        error = asG12Failure(markError, operation: '标记已读');
      }
      _startWatch();
    } catch (failure) {
      if (!_disposed) error = asG12Failure(failure, operation: '加载会话');
    } finally {
      busy = false;
      _safeNotify();
    }
  }

  Future<void> refresh() async {
    error = null;
    _safeNotify();
    try {
      final page = await port.history(conv, 0, pageSize);
      final outbox = await port.outboxStatus();
      if (_disposed) return;
      _mergeMessages(page.messages);
      fromCache = page.fromCache;
      _applyOutbox(outbox);
      if (!fromCache) {
        try {
          await port.markRead(conv, 0);
        } catch (_) {
          // 已读失败只影响红点，下次轮询会再试。
        }
      }
    } catch (failure) {
      if (!_disposed) error = asG12Failure(failure, operation: '刷新消息');
    } finally {
      _safeNotify();
    }
  }

  Future<void> loadOlder() async {
    if (!canLoadOlder) return;
    loadingOlder = true;
    _safeNotify();
    try {
      final page = await port.history(conv, nextBeforeSeq ?? 0, pageSize);
      if (_disposed) return;
      _mergeMessages(page.messages);
      nextBeforeSeq = page.nextBeforeSeq;
    } catch (failure) {
      if (!_disposed) error = asG12Failure(failure, operation: '加载更早消息');
    } finally {
      loadingOlder = false;
      _safeNotify();
    }
  }

  /// 发送文本。返回 null 表示异常（[error] 已设置）；否则是 Rust 的终态。
  Future<SendOutcome?> send(String text) async {
    if (sending) return null;
    final body = text.trim();
    if (body.isEmpty) return null;
    sending = true;
    error = null;
    _safeNotify();
    try {
      final outcome = await port.sendText(conv, body);
      if (_disposed) return outcome;
      if (outcome.state == OutboxState.sent) {
        // 发送成功后重新查询：服务端 seq 才是列表的唯一真相。
        await refresh();
      } else {
        await _refreshOutbox();
      }
      return outcome;
    } catch (failure) {
      if (!_disposed) error = asG12Failure(failure, operation: '发送消息');
      return null;
    } finally {
      sending = false;
      _safeNotify();
    }
  }

  Future<void> retry(String taskId) async {
    if (!retrying.add(taskId)) return;
    _safeNotify();
    try {
      await port.retryOutbox(taskId);
      // 不能只清 pending：重新问历史与队列，确认服务端真实终态。
      await refresh();
    } catch (failure) {
      if (!_disposed) error = asG12Failure(failure, operation: '重试发送');
    } finally {
      retrying.remove(taskId);
      _safeNotify();
    }
  }

  Future<void> drop(String taskId) async {
    if (!retrying.add(taskId)) return;
    _safeNotify();
    try {
      await port.dropOutbox(taskId);
      await _refreshOutbox();
    } catch (failure) {
      if (!_disposed) error = asG12Failure(failure, operation: '删除待发消息');
    } finally {
      retrying.remove(taskId);
      _safeNotify();
    }
  }

  /// 前台恢复 / 网络恢复：先泵队列，再重查历史和队列。
  Future<void> onSignal(ChatSignal signal) async {
    if (!_foreground) return;
    try {
      await port.flushOutbox();
    } catch (failure) {
      if (!_disposed) error = asG12Failure(failure, operation: '补发离线消息');
    }
    await refresh();
  }

  void setForeground(bool foreground) {
    if (_foreground == foreground) return;
    _foreground = foreground;
    if (foreground) {
      _startWatch();
      unawaited(refresh());
    } else {
      _sub?.cancel();
      _sub = null;
    }
  }

  void _startWatch() {
    if (_disposed || !_foreground) return;
    _sub?.cancel();
    _sub = port.watch(conv, interval: watchInterval).listen(_onEvent);
  }

  void _onEvent(ChatEvent event) {
    if (_disposed) return;
    switch (event) {
      case MessagesUpdated(:final messages, :final fromCache):
        final before = _latestSeq;
        _mergeMessages(messages);
        this.fromCache = fromCache;
        _safeNotify();
        if (_latestSeq > before && !fromCache) {
          unawaited(_markLatestRead());
        }
      case UnreadUpdated(:final unread):
        if (firstUnreadSeq == null && unread.unread > 0) {
          final first = unread.headSeq - unread.unread + 1;
          if (first > 0) firstUnreadSeq = first;
        }
        _safeNotify();
      case OutboxUpdated(:final summary):
        _applyOutbox(summary);
        _safeNotify();
      case ChatWatchFailed(:final failure):
        error = failure;
        _safeNotify();
      case ConversationsUpdated():
        break;
    }
  }

  Future<void> _markLatestRead() async {
    try {
      await port.markRead(conv, 0);
    } catch (_) {
      // 轮询下一轮会再试，不覆盖更重要的加载错误。
    }
  }

  void _mergeMessages(List<ChatMessage> incoming) {
    if (incoming.isEmpty) return;
    final byId = <String, ChatMessage>{
      for (final message in messages) message.msgId: message,
    };
    for (final message in incoming) {
      byId[message.msgId] = message;
      if (message.seq > _latestSeq) _latestSeq = message.seq;
    }
    final merged = byId.values.toList()
      ..sort((a, b) {
        final bySeq = a.seq.compareTo(b.seq);
        return bySeq != 0 ? bySeq : a.msgId.compareTo(b.msgId);
      });
    messages = merged.length > maxMessages
        ? List<ChatMessage>.unmodifiable(merged.sublist(merged.length - maxMessages))
        : List<ChatMessage>.unmodifiable(merged);
  }

  void _applyOutbox(OutboxSummary summary) {
    pending = List<OutboxEntry>.unmodifiable(
      summary.entries.where((entry) => entry.conv == conv),
    );
  }

  Future<void> _refreshOutbox() async {
    final summary = await port.outboxStatus();
    if (_disposed) return;
    _applyOutbox(summary);
  }

  void _safeNotify() {
    if (!_disposed) notifyListeners();
  }

  @override
  void dispose() {
    _disposed = true;
    _sub?.cancel();
    super.dispose();
  }
}
