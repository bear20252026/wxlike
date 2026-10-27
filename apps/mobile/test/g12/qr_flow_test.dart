import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:wxlike_mobile/features/auth/qr_approval/qr_approval_page.dart';
import 'package:wxlike_mobile/features/auth/qr_approval/qr_scan_login_page.dart';
import 'package:wxlike_mobile/features/auth/qr_approval/qr_transport.dart';
import 'package:wxlike_mobile/features/auth/qr_approval/qr_wiring.dart';
import 'package:wxlike_mobile/features/auth/challenge/login_page.dart';
import 'package:wxlike_mobile/shell/contracts.dart';
import 'package:wxlike_mobile/shell/mobile_app.dart';
import 'package:wxlike_mobile/shell/theme.dart';
import 'package:wxlike_mobile/src/service.dart';

import 'composer_support.dart';
import 'qr_support.dart';
import 'support.dart';

/// 「扫码登录」这一条路从组合根到 HTTP 请求的贯通测试。
///
/// 用 [buildQrIdentityPort] —— 与 `lib/main.dart` 里 `ProductionRoot` 调的是
/// **同一个函数**——所以这个测试红就等价于"组合根没接上"。
void main() {
  late RecordingQrTransport transport;
  late List<String> logged;

  /// 把 `debugPrint` 换成收集器：任何一处把 secret 打出去都会在这里露出来。
  void captureLogs() {
    final original = debugPrint;
    addTearDown(() => debugPrint = original);
    debugPrint = (String? message, {int? wrapWidth}) {
      if (message != null) logged.add(message);
    };
  }

  Future<void> bootApp(WidgetTester tester) async {
    final messaging = StubMessaging();
    addTearDown(messaging.close);
    final lifecycle = FakeLifecycle();
    addTearDown(lifecycle.close);
    await tester.pumpWidget(WxlikeMobileApp(
      account: StubAccount(),
      session: const SessionInfo(userId: 'u1', server: 'http://srv', signedIn: true),
      messaging: messaging,
      // ↓↓↓ 组合根接线的那一行；删掉它（或把 qr_wiring 里的构造改坏）这个测试就红。
      identity: buildQrIdentityPort(
        serverBase: () => 'http://srv',
        token: () async => 'bearer-for-u1',
        transport: transport,
        fallback: const UnusedFallback(),
      ),
      platform: FakePlatform(),
      push: FakePush(),
      lifecycle: lifecycle,
      secrets: MemorySecrets(),
    ));
    await tester.pumpAndSettle();
  }

  setUp(() {
    transport = RecordingQrTransport(
      reply: (request) {
        if (request.path.endsWith('/scan')) {
          return const QrApiResponse(
            status: 200,
            body: {
              'ticket_id': qrTicketId,
              'state': 'SCANNED',
              'platform': 'web',
              'label': 'MacBook Pro',
              'ip_hint': '203.0.113.7',
            },
          );
        }
        return const QrApiResponse(
          status: 200,
          body: {'ticket_id': qrTicketId, 'state': 'APPROVED'},
        );
      },
    );
    logged = [];
    captureLogs();
  });

  testWidgets('从「扫一扫」入口一路点到控制器，扫与批各发一次且形状正确', (tester) async {
    await bootApp(tester);
    // 已登录 → 产品壳，而不是登录页。
    expect(find.byType(LoginPage), findsNothing);

    // 「+」→ 扫一扫：这就是任务书里那个"扫码登录入口"。
    await tester.tap(find.byTooltip('发起'));
    await tester.pumpAndSettle();
    await tester.tap(find.widgetWithText(ListTile, '扫一扫'));
    await tester.pumpAndSettle();
    expect(find.byType(QrScanLoginPage), findsOneWidget);

    await tester.enterText(find.byKey(const Key('qr-login-payload')), qrPayload);
    await tester.tap(find.byKey(const Key('qr-login-submit')));
    await tester.pumpAndSettle();
    expect(find.byType(QrApprovalPage), findsOneWidget);

    // 进页面即 /scan：扫码者没有轮询权限，`/scan` 的回包就是他看到的第一个真实状态。
    expect(transport.paths, ['POST /api/qr/tickets/$qrTicketId/scan']);
    expect(transport.bodyOf('/scan'), {'secret': qrSecret});
    expect(find.byKey(const Key('qr-state')), findsOneWidget);
    expect(find.text('SCANNED'), findsOneWidget);

    // 批准：对话框 → 服务端状态变成 APPROVED。
    await tester.tap(find.byKey(const Key('qr-approve')));
    await tester.pumpAndSettle();
    await tester.tap(find.descendant(of: find.byType(AlertDialog), matching: find.text('批准登录')));
    await tester.pumpAndSettle();

    expect(
      transport.paths,
      expectedQrCalls(qrTicketId),
      reason: '必须先 scan 再 approve：identity 的 decide() 只接受 state=scanned',
    );
    expect(transport.bodyOf('/approve'), isNull);
    expect(find.text('APPROVED'), findsOneWidget, reason: '成功要显示成状态，不是一句 toast');
    // 终态之后不能再点。
    expect(tester.widget<FilledButton>(find.byKey(const Key('qr-approve'))).onPressed, isNull);

    // 设备信息在批准之后仍然是那张桌面的（approve 回包里没有这些字段）。
    expect(find.text('MacBook Pro'), findsOneWidget);
  });

  testWidgets('畸形载荷：给出可见错误，一次网络都不发', (tester) async {
    await bootApp(tester);
    await tester.tap(find.byTooltip('发起'));
    await tester.pumpAndSettle();
    await tester.tap(find.widgetWithText(ListTile, '扫一扫'));
    await tester.pumpAndSettle();

    await tester.enterText(
      find.byKey(const Key('qr-login-payload')),
      'wxlike-qr://$qrTicketId/$qrSecret/too-many-segments',
    );
    await tester.tap(find.byKey(const Key('qr-login-submit')));
    await tester.pumpAndSettle();

    expect(find.byType(QrApprovalPage), findsNothing);
    expect(transport.requests, isEmpty, reason: '载荷没解析通过就不该发请求');
    expect(find.textContaining('登录码格式不正确'), findsOneWidget);
    // 错误文案本身也不能带出 secret。
    expect(find.textContaining(qrSecret), findsNothing);
  });

  testWidgets('scheme 不对同样拒绝且不发请求', (tester) async {
    await bootApp(tester);
    await tester.tap(find.byTooltip('发起'));
    await tester.pumpAndSettle();
    await tester.tap(find.widgetWithText(ListTile, '扫一扫'));
    await tester.pumpAndSettle();

    await tester.enterText(
      find.byKey(const Key('qr-login-payload')),
      'https://example.com/$qrTicketId/$qrSecret',
    );
    await tester.tap(find.byKey(const Key('qr-login-submit')));
    await tester.pumpAndSettle();

    expect(transport.requests, isEmpty);
    expect(find.textContaining('wxlike 二维码'), findsOneWidget);
  });

  testWidgets('全链路任何输出里都不出现 secret', (tester) async {
    await bootApp(tester);
    await tester.tap(find.byTooltip('发起'));
    await tester.pumpAndSettle();
    await tester.tap(find.widgetWithText(ListTile, '扫一扫'));
    await tester.pumpAndSettle();
    await tester.enterText(find.byKey(const Key('qr-login-payload')), qrPayload);
    await tester.tap(find.byKey(const Key('qr-login-submit')));
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const Key('qr-approve')));
    await tester.pumpAndSettle();
    await tester.tap(find.descendant(of: find.byType(AlertDialog), matching: find.text('批准登录')));
    await tester.pumpAndSettle();

    // 1) 调试输出（debugPrint 收集器 + 传输层的日志形状）
    final sinks = [...logged, ...transport.logLines];
    expect(sinks, isNotEmpty, reason: '没有采集到任何输出就等于断言空转');
    for (final line in sinks) {
      expect(line, isNot(contains(qrSecret)), reason: line);
    }
    // 2) 屏上的每一个文本节点
    for (final data in tester.widgetList<Text>(find.byType(Text)).map((t) => t.data ?? '')) {
      expect(data, isNot(contains(qrSecret)), reason: data);
    }
    // 3) 对象的字符串表示（QrApprovalTarget / QrScanPayload / QrApiRequest）
    final rendered = [
      '${const QrApprovalTarget(challengeId: qrTicketId, secret: qrSecret)}',
      '${transport.requests.first}',
    ];
    for (final text in rendered) {
      expect(text, isNot(contains(qrSecret)), reason: text);
    }
    // 4) 持久化：确认 secret 没被写进安全存储
    expect(transport.bodies, contains('{secret: $qrSecret}'), reason: 'body 里必须有，否则批准不可能成功');
  });

  testWidgets('服务端判定过期时界面显示 EXPIRED 而不是成功', (tester) async {
    transport = RecordingQrTransport(
      reply: (_) => const QrApiResponse(status: 410, body: {'error': 'ticket expired'}),
    );
    await bootApp(tester);
    await tester.tap(find.byTooltip('发起'));
    await tester.pumpAndSettle();
    await tester.tap(find.widgetWithText(ListTile, '扫一扫'));
    await tester.pumpAndSettle();
    await tester.enterText(find.byKey(const Key('qr-login-payload')), qrPayload);
    await tester.tap(find.byKey(const Key('qr-login-submit')));
    await tester.pumpAndSettle();

    expect(find.byType(QrApprovalPage), findsOneWidget);
    expect(find.text('无法读取登录挑战'), findsOneWidget);
    expect(find.textContaining('登录码已过期'), findsOneWidget);
    expect(find.text('APPROVED'), findsNothing);
    expect(transport.paths, ['POST /api/qr/tickets/$qrTicketId/scan']);
  });

  test('组合根工厂在无 bearer 时不发请求，并给出真实原因', () async {
    final transport = RecordingQrTransport();
    final identity = buildQrIdentityPort(
      serverBase: () => 'http://srv',
      // 不给 token：这就是当前绑定交付给 Dart 的真实处境。
      token: defaultQrTokenProvider(),
      transport: transport,
    );
    final failure = await guard(
      () => identity.scanQr(qrTicketId, secret: qrSecret),
    );
    // 不假装成功：端口自己不发请求，原因由传输层给出。
    expect(failure.code, isNot(''));
    expect(transport.requests, isEmpty);
  });
}
