import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:wxlike_mobile/screens/chat.dart';
import 'package:wxlike_mobile/screens/conversations.dart';
import 'package:wxlike_mobile/screens/login.dart';
import 'package:wxlike_mobile/src/service.dart';

/// 界面测试用的替身。
///
/// 它替换的是 **Rust 的返回值**，不是 Rust 的行为——真实的返回形状与错误分类由
/// `test/ffi_smoke_test.dart` 直接过 FFI 验（那边没有替身）。这里只是让三屏在
/// 没有 `.so` 的测试机上也能被点一遍。
class StubService implements WxlikeService {
  StubService({this.online = true, this.sendState = OutboxState.pending, this.failLogin = false});

  bool online;
  OutboxState sendState;
  bool failLogin;
  final List<String> sent = [];
  int logins = 0;

  @override
  SessionInfo session() => const SessionInfo(userId: 'alice', server: 'stub', signedIn: true);

  @override
  Future<SessionInfo> login(String name, String password) async {
    logins++;
    if (failLogin) {
      throw const WxlikeErrorExceptionUnauthorized(detail: '用户名或密码不对');
    }
    return session();
  }

  @override
  Future<SessionInfo> register(String name, String password) async => session();

  @override
  Future<void> logout() async {}

  @override
  Future<LiveInfo> connect() async => liveInfo();

  @override
  Future<LiveInfo> liveInfo() async => LiveInfo(
        state: online ? LiveState.online : LiveState.offline,
        userId: 'alice',
        server: 'stub',
        pending: sendState == OutboxState.pending ? 1 : 0,
        pumpError: '',
      );

  @override
  Future<void> disconnect() async {}

  @override
  Future<ConversationList> conversations() async => ConversationList(
        fromCache: !online,
        conversations: const [
        ConversationRow(
          conv: 'g-1',
          kind: 'group',
          title: '项目组',
          members: 3,
          unread: 2,
          pending: 0,
        ),
        ConversationRow(
          conv: 'd-alice~bob',
          kind: 'direct',
          title: 'bob',
          members: 0,
          unread: 1,
          pending: 1,
        ),
      ],
      );

  ChatMessage _m(int seq, String sender, String text) => ChatMessage(
        conv: 'd-alice~bob',
        seq: seq,
        msgId: 'm$seq',
        sender: sender,
        kind: 'text',
        text: text,
        mediaId: '',
        sentAtMs: 1700000000000 + seq,
        withdrawn: false,
        mine: sender == 'alice',
      );

  @override
  Future<MessagePage> history(String conv, int beforeSeq, int limit) async => MessagePage(
        messages: [_m(1, 'bob', '在吗'), _m(2, 'alice', '在')],
        fromCache: !online,
        nextBeforeSeq: -1,
      );

  @override
  Future<UnreadState> unread(String conv) async => const UnreadState(
        conv: 'd-alice~bob',
        owner: 'direct',
        headSeq: 2,
        readSeq: 1,
        unread: 1,
      );

  @override
  Future<UnreadState> markRead(String conv, int uptoSeq) async => unread(conv);

  @override
  Future<SendOutcome> sendText(String conv, String text) async {
    sent.add(text);
    return SendOutcome(
      taskId: 't${sent.length}',
      state: sendState,
      msgId: sendState == OutboxState.sent ? 'srv-1' : '',
      error: sendState == OutboxState.sent ? '' : '连不上服务器',
    );
  }

  @override
  Future<OutboxSummary> outboxStatus() async => OutboxSummary(
        pending: sendState == OutboxState.pending ? 1 : 0,
        failed: sendState == OutboxState.failed ? 1 : 0,
        entries: [
          if (sendState != OutboxState.sent)
            OutboxEntry(
              taskId: 't1',
              conv: 'd-alice~bob',
              kind: 'text',
              text: '要发的话',
              state: sendState,
              attempts: 0,
              nextRetryAt: 0,
              lastError: '连不上服务器',
            ),
        ],
      );

  @override
  Future<FlushReport> flushOutbox() async => const FlushReport(sent: 0, retrying: 0, failed: 0);

  @override
  Future<OutboxSummary> retryOutbox(String taskId) async => outboxStatus();

  @override
  Future<bool> dropOutbox(String taskId) async => true;

  @override
  Future<void> registerPushToken(String platform, String token) async {}

  @override
  void close() {}
}

Widget _app(Widget child) => MaterialApp(home: child);

void main() {
  testWidgets('登录成功后进入会话列表', (tester) async {
    final service = StubService();
    await tester.pumpWidget(_app(LoginScreen(service: service)));

    await tester.enterText(find.byType(TextField).at(1), 'alice');
    await tester.enterText(find.byType(TextField).at(2), 'pw');
    // enterText 触发的是 setState；不 pump 一下，按钮还是"禁用"那个旧 element
    await tester.pump();
    await tester.tap(find.text('登录'));
    await tester.pumpAndSettle();

    expect(service.logins, 1);
    expect(find.text('项目组'), findsOneWidget);
    expect(find.text('bob'), findsOneWidget);
    // 未读是 Rust 给的数字；-1 时列表必须写"未读未知"而不是 0
    expect(find.text('2'), findsOneWidget);
    expect(find.textContaining('在线'), findsWidgets);
  });

  testWidgets('登录失败把错误类说出来，不静默回到输入页', (tester) async {
    final service = StubService(failLogin: true);
    await tester.pumpWidget(_app(LoginScreen(service: service)));

    await tester.enterText(find.byType(TextField).at(1), 'alice');
    await tester.enterText(find.byType(TextField).at(2), 'pw');
    await tester.pump();
    await tester.tap(find.text('登录'));
    await tester.pumpAndSettle();

    expect(service.logins, 1);
    expect(find.textContaining('需要重新登录'), findsOneWidget);
    expect(find.byType(ConversationsScreen), findsNothing);
  });

  testWidgets('离线时列表显示离线状态', (tester) async {
    final service = StubService(online: false);
    await tester.pumpWidget(_app(ConversationsScreen(service: service, session: service.session())));
    await tester.pumpAndSettle();

    // 顶栏的"离线"点 + 列表上的"这是本地缓存"提示，两个都要有
    expect(find.textContaining('离线'), findsWidgets);
    expect(find.text('离线中 · 下面是本地缓存的会话'), findsOneWidget);
    expect(find.textContaining('1 条待发送'), findsOneWidget);
  });

  testWidgets('会话点得进聊天页，历史按 mine 分左右', (tester) async {
    final service = StubService();
    await tester.pumpWidget(_app(ConversationsScreen(service: service, session: service.session())));
    await tester.pumpAndSettle();

    await tester.tap(find.text('bob'));
    await tester.pumpAndSettle();

    expect(find.byType(ChatScreen), findsOneWidget);
    expect(find.text('在吗'), findsOneWidget);
    expect(find.text('在'), findsOneWidget);
  });

  testWidgets('发不出去的消息留在屏上标"待发送"，并能重试', (tester) async {
    final service = StubService(sendState: OutboxState.pending);
    await tester.pumpWidget(_app(ChatScreen(
      service: service,
      row: const ConversationRow(
        conv: 'd-alice~bob',
        kind: 'direct',
        title: 'bob',
        members: 0,
        unread: 1,
        pending: 1,
      ),
    )));
    await tester.pumpAndSettle();

    await tester.enterText(find.byType(TextField).first, '要发的话');
    await tester.tap(find.text('发送'));
    await tester.pumpAndSettle();

    expect(service.sent, ['要发的话']);
    expect(find.textContaining('待发送'), findsOneWidget);
    expect(find.text('重试'), findsOneWidget);
    expect(find.text('删除'), findsOneWidget);
  });

  testWidgets('服务端明确拒绝时标"发送失败"并给出原因', (tester) async {
    final service = StubService(sendState: OutboxState.failed);
    await tester.pumpWidget(_app(ChatScreen(
      service: service,
      row: const ConversationRow(
        conv: 'd-alice~bob',
        kind: 'direct',
        title: 'bob',
        members: 0,
        unread: 0,
        pending: 0,
      ),
    )));
    await tester.pumpAndSettle();

    await tester.enterText(find.byType(TextField).first, '不该发');
    await tester.tap(find.text('发送'));
    await tester.pumpAndSettle();

    expect(find.textContaining('发送失败'), findsOneWidget);
    expect(find.textContaining('连不上服务器'), findsOneWidget);
  });
}
