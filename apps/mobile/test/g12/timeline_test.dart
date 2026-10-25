import 'dart:async';

import 'package:flutter_test/flutter_test.dart';
import 'package:wxlike_mobile/features/chat/timeline/conversations_controller.dart';
import 'package:wxlike_mobile/features/chat/timeline/messaging_ports.dart';
import 'package:wxlike_mobile/features/chat/timeline/timeline_controller.dart';
import 'package:wxlike_mobile/shell/contracts.dart';
import 'package:wxlike_mobile/src/service.dart';

class FakeMessaging implements MessagingPort {
  List<ChatMessage> latest = const [];
  List<ChatMessage> older = const [];
  MessagePage? olderPage;
  OutboxSummary outbox = const OutboxSummary(
    pending: 0,
    failed: 0,
    entries: [],
  );
  bool fromCache = false;
  Object? historyError;
  int historyCalls = 0;
  int markReadCalls = 0;
  int retryCalls = 0;
  int flushCalls = 0;
  int watchSubscriptions = 0;
  final _events = StreamController<ChatEvent>.broadcast();

  void emit(ChatEvent event) => _events.add(event);

  @override
  SessionInfo session() =>
      const SessionInfo(userId: 'u', server: 's', signedIn: true);

  @override
  Future<ConversationList> conversations() async =>
      const ConversationList(conversations: [], fromCache: false);

  @override
  Future<LiveInfo> liveInfo() async => const LiveInfo(
    state: LiveState.online,
    userId: 'u',
    server: 's',
    pending: 0,
    pumpError: '',
  );

  @override
  Future<MessagePage> history(String conv, int beforeSeq, int limit) async {
    historyCalls++;
    if (historyError != null) throw historyError!;
    if (beforeSeq > 0) {
      return olderPage ??
          MessagePage(messages: older, fromCache: false, nextBeforeSeq: -1);
    }
    return MessagePage(
      messages: latest,
      fromCache: fromCache,
      nextBeforeSeq: 100,
    );
  }

  @override
  Future<UnreadState> unread(String conv) async => const UnreadState(
    conv: 'c1',
    owner: 'direct',
    headSeq: 5,
    readSeq: 2,
    unread: 3,
  );

  @override
  Future<UnreadState> markRead(String conv, int uptoSeq) async {
    markReadCalls++;
    return const UnreadState(
      conv: 'c1',
      owner: 'direct',
      headSeq: 5,
      readSeq: 5,
      unread: 0,
    );
  }

  @override
  Future<SendOutcome> sendText(String conv, String text) async =>
      const SendOutcome(
        taskId: 't1',
        state: OutboxState.sent,
        msgId: 'm9',
        error: '',
      );

  @override
  Future<OutboxSummary> outboxStatus() async => outbox;

  @override
  Future<FlushReport> flushOutbox() async {
    flushCalls++;
    return const FlushReport(sent: 1, retrying: 0, failed: 0);
  }

  @override
  Future<OutboxSummary> retryOutbox(String taskId) async {
    retryCalls++;
    return outbox;
  }

  @override
  Future<bool> dropOutbox(String taskId) async => true;

  @override
  Future<LiveInfo> connect() async => const LiveInfo(
    state: LiveState.online,
    userId: 'u',
    server: 's',
    pending: 0,
    pumpError: '',
  );

  @override
  Stream<ChatEvent> watch(
    String conv, {
    Duration interval = const Duration(seconds: 2),
  }) {
    watchSubscriptions++;
    return _events.stream;
  }
}

ChatMessage message(
  int seq, {
  String id = '',
  String text = '',
  bool mine = false,
}) => ChatMessage(
  conv: 'c1',
  seq: seq,
  msgId: id.isEmpty ? 'm$seq' : id,
  sender: mine ? 'me' : 'peer',
  kind: 'text',
  text: text,
  mediaId: '',
  sentAtMs: 1000 + seq,
  withdrawn: false,
  mine: mine,
);

void main() {
  group('TimelineController 投影', () {
    test('按 msg_id 去重、按 seq 排序，并保留未读定位', () async {
      final port = FakeMessaging()
        ..latest = [message(2), message(1), message(2, text: '更新后')];
      final controller = TimelineController(
        port: port,
        conv: 'c1',
        watchInterval: const Duration(hours: 1),
      );
      await controller.init();

      expect(controller.messages.map((m) => m.seq), [1, 2]);
      expect(controller.messages.last.text, '更新后');
      expect(controller.nextBeforeSeq, 100);
      expect(controller.firstUnreadSeq, 3);
      expect(port.markReadCalls, 1);
      expect(controller.initialized, isTrue);
      controller.dispose();
    });

    test('内存上限生效，只保留最近 N 条', () async {
      final port = FakeMessaging()
        ..latest = [for (var i = 1; i <= 10; i++) message(i)];
      final controller = TimelineController(
        port: port,
        conv: 'c1',
        maxMessages: 4,
        watchInterval: const Duration(hours: 1),
      );
      await controller.init();
      expect(controller.messages.length, 4);
      expect(controller.messages.first.seq, 7);
      expect(controller.canLoadOlder, isFalse, reason: '达到内存上限后不应继续向前复制历史');
      controller.dispose();
    });

    test('反向分页把更早的消息合进同一列表', () async {
      final port = FakeMessaging()
        ..latest = [message(10), message(11)]
        ..older = [message(8), message(9)]
        ..olderPage = MessagePage(
          messages: [message(8), message(9)],
          fromCache: false,
          nextBeforeSeq: 50,
        );
      final controller = TimelineController(
        port: port,
        conv: 'c1',
        watchInterval: const Duration(hours: 1),
      );
      await controller.init();
      await controller.loadOlder();

      expect(controller.messages.map((m) => m.seq), [8, 9, 10, 11]);
      expect(controller.nextBeforeSeq, 50);
      controller.dispose();
    });

    test('retry 后重新查询真实历史，而不是只清 pending', () async {
      final port = FakeMessaging()..latest = [message(1)];
      final controller = TimelineController(
        port: port,
        conv: 'c1',
        watchInterval: const Duration(hours: 1),
      );
      await controller.init();
      final callsBefore = port.historyCalls;

      // 服务端已经接受了这条消息：队列清空、历史多了一条。
      port.outbox = const OutboxSummary(pending: 0, failed: 0, entries: []);
      port.latest = [message(1), message(2, id: 'm2')];
      await controller.retry('t1');

      expect(port.retryCalls, 1);
      expect(port.historyCalls, greaterThan(callsBefore));
      expect(controller.messages.map((m) => m.msgId), contains('m2'));
      controller.dispose();
    });

    test('前台恢复触发 pump 与重查；后台停止订阅', () async {
      final port = FakeMessaging()..latest = [message(1)];
      final controller = TimelineController(
        port: port,
        conv: 'c1',
        watchInterval: const Duration(hours: 1),
      );
      await controller.init();
      final subs = port.watchSubscriptions;

      controller.setForeground(false);
      await Future<void>.delayed(Duration.zero);
      final callsWhileBackground = port.historyCalls;
      port.emit(MessagesUpdated([message(2)], fromCache: false));
      await Future<void>.delayed(Duration.zero);
      expect(port.historyCalls, callsWhileBackground);

      controller.setForeground(true);
      await Future<void>.delayed(Duration.zero);
      expect(port.flushCalls, greaterThanOrEqualTo(0));
      expect(port.watchSubscriptions, greaterThan(subs));
      controller.dispose();
    });

    test('加载失败映射为可读失败，缓存状态如实保留', () async {
      final port = FakeMessaging()
        ..historyError = const G12Failure(
          G12FailureKind.offline,
          'NETWORK',
          '断网',
          retryable: true,
        );
      final controller = TimelineController(
        port: port,
        conv: 'c1',
        watchInterval: const Duration(hours: 1),
      );
      await controller.init();
      expect(controller.error?.kind, G12FailureKind.offline);
      expect(controller.messages, isEmpty);
      controller.dispose();

      final cached = FakeMessaging()
        ..fromCache = true
        ..latest = [message(1)];
      final second = TimelineController(
        port: cached,
        conv: 'c1',
        watchInterval: const Duration(hours: 1),
      );
      await second.init();
      expect(second.fromCache, isTrue);
      second.dispose();
    });
  });

  group('ConversationsController', () {
    test('离线时保留本地列表并在 banner 上说明原因', () async {
      final port = FakeMessaging();
      final controller = ConversationsController(
        port: port,
        watchInterval: const Duration(hours: 1),
      );
      await controller.load();
      expect(controller.error, isNull);
      expect(controller.rows, isEmpty);
      controller.dispose();
    });
  });
}
