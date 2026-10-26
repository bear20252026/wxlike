import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:wxlike_mobile/features/auth/auth_ports.dart';
import 'package:wxlike_mobile/features/auth/challenge/login_page.dart';
import 'package:wxlike_mobile/features/chat/timeline/messaging_ports.dart';
import 'package:wxlike_mobile/shell/mobile_app.dart';
import 'package:wxlike_mobile/src/service.dart';

import 'composer_support.dart';
import 'support.dart';

/// 账号端口的替身：登录/登出改写本地会话，让壳真的发生切换。
class FakeAccount implements AccountPort {
  FakeAccount(this._session);

  SessionInfo _session;
  int logins = 0;
  int logouts = 0;

  @override
  SessionInfo session() => _session;

  @override
  Future<SessionInfo> login(String name, String password) async {
    logins++;
    _session = SessionInfo(userId: name, server: 'stub', signedIn: true);
    return _session;
  }

  @override
  Future<SessionInfo> register(String name, String password) async => login(name, password);

  @override
  Future<void> logout() async {
    logouts++;
    _session = const SessionInfo(userId: '', server: '', signedIn: false);
  }

  @override
  Future<LiveInfo> connect() async => const LiveInfo(
    state: LiveState.online,
    userId: 'u',
    server: 'stub',
    pending: 0,
    pumpError: '',
  );

  @override
  Future<LiveInfo> liveInfo() async => const LiveInfo(
    state: LiveState.online,
    userId: 'u',
    server: 'stub',
    pending: 0,
    pumpError: '',
  );

  @override
  Future<void> registerPushToken(String platform, String token) async {}
}

/// 消息端口的替身：空列表即可，壳只要求 watch 不抛错。
class EmptyMessaging implements MessagingPort {
  final _events = StreamController<ChatEvent>.broadcast();

  void close() => _events.close();

  @override
  SessionInfo session() => const SessionInfo(userId: 'u', server: 'stub', signedIn: true);

  @override
  Future<ConversationList> conversations() async =>
      const ConversationList(conversations: [], fromCache: false);

  @override
  Future<LiveInfo> liveInfo() async => const LiveInfo(
    state: LiveState.online,
    userId: 'u',
    server: 'stub',
    pending: 0,
    pumpError: '',
  );

  @override
  Future<MessagePage> history(String conv, int beforeSeq, int limit) async =>
      const MessagePage(messages: [], fromCache: false, nextBeforeSeq: -1);

  @override
  Future<UnreadState> unread(String conv) async => const UnreadState(
    conv: 'c1',
    owner: 'direct',
    headSeq: 0,
    readSeq: 0,
    unread: 0,
  );

  @override
  Future<UnreadState> markRead(String conv, int uptoSeq) async => unread(conv);

  @override
  Future<SendOutcome> sendText(String conv, String text) async => const SendOutcome(
    taskId: 't1',
    state: OutboxState.sent,
    msgId: 'm1',
    error: '',
  );

  @override
  Future<OutboxSummary> outboxStatus() async =>
      const OutboxSummary(pending: 0, failed: 0, entries: []);

  @override
  Future<FlushReport> flushOutbox() async => const FlushReport(sent: 0, retrying: 0, failed: 0);

  @override
  Future<OutboxSummary> retryOutbox(String taskId) async =>
      const OutboxSummary(pending: 0, failed: 0, entries: []);

  @override
  Future<bool> dropOutbox(String taskId) async => true;

  @override
  Future<LiveInfo> connect() async => liveInfo();

  @override
  Stream<ChatEvent> watch(String conv, {Duration interval = const Duration(seconds: 2)}) =>
      _events.stream;
}

void main() {
  testWidgets('组合根接线：登录页 → 账号登录 → 产品壳 → 退出回到登录页', (tester) async {
    final account = FakeAccount(const SessionInfo(userId: '', server: 'stub', signedIn: false));
    final messaging = EmptyMessaging();
    final lifecycle = FakeLifecycle();

    await tester.pumpWidget(WxlikeMobileApp(
      account: account,
      session: account.session(),
      messaging: messaging,
      platform: FakePlatform(),
      push: FakePush(),
      lifecycle: lifecycle,
      secrets: MemorySecrets(),
    ));
    await tester.pumpAndSettle();

    // 未登录：落在登录页，账号密码通道可见（发布版也必须能进）。
    expect(find.byType(LoginPage), findsOneWidget);

    await tester.tap(find.text('更多登录方式'));
    await tester.pumpAndSettle();
    final accountPage = find.byType(AccountLoginPage);
    expect(accountPage, findsOneWidget);

    // 登录页本身还有手机号/验证码两个输入框，必须把查找限制在账号页内：
    // 0 = 服务器地址，1 = 用户名，2 = 密码
    final fields = find.descendant(of: accountPage, matching: find.byType(TextField));
    await tester.enterText(fields.at(1), 'alice');
    await tester.enterText(fields.at(2), 'pw');
    await tester.pump();
    await tester.tap(
      find.descendant(of: accountPage, matching: find.widgetWithText(FilledButton, '登录')),
    );
    await tester.pumpAndSettle();

    expect(account.logins, 1);
    // 产品壳：底部四个 tab 就位（「微信」同时是会话页标题，故按 tab 控件找）
    expect(find.widgetWithText(NavigationDestination, '微信'), findsOneWidget);
    expect(find.widgetWithText(NavigationDestination, '通讯录'), findsOneWidget);
    expect(find.widgetWithText(NavigationDestination, '我'), findsOneWidget);

    // 「我」→ 退出登录 → 确认对话框
    await tester.tap(find.text('我'));
    await tester.pumpAndSettle();
    await tester.tap(find.text('退出登录'));
    await tester.pumpAndSettle();
    await tester.tap(find.widgetWithText(FilledButton, '退出'));
    await tester.pumpAndSettle();

    expect(account.logouts, 1);
    expect(find.byType(LoginPage), findsOneWidget);

    lifecycle.close();
    messaging.close();
  });
}
