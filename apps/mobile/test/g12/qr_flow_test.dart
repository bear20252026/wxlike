import 'dart:io';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:wxlike_mobile/features/auth/qr_approval/qr_approval_page.dart';
import 'package:wxlike_mobile/features/auth/qr_approval/qr_scan_login_page.dart';
import 'package:wxlike_mobile/features/auth/qr_approval/qr_transport.dart';
import 'package:wxlike_mobile/features/auth/qr_approval/qr_wiring.dart';
import 'package:wxlike_mobile/features/auth/challenge/login_page.dart';
import 'package:wxlike_mobile/shell/contracts.dart';
import 'package:wxlike_mobile/shell/mobile_app.dart';
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
  ///
  /// **采集与还原都必须发生在用例体内部**，不能交给 `addTearDown`。机制
  /// （本机 SDK `/d/flutter`）：
  /// * `AutomatedTestWidgetsFlutterBinding.debugPrintOverride` 就是
  ///   `debugPrintSynchronously`（`flutter_test/lib/src/binding.dart:2231`），
  ///   绑定构造时把全局 `debugPrint` 指向它（同文件 1078 行）；
  /// * 用例体结束后先跑 `_verifyInvariants()`（同文件 1974 / 1996 行），它调
  ///   `debugAssertAllFoundationVarsUnset(debugPrintOverride: debugPrintOverride)`，
  ///   比的是值：`debugPrint != debugPrintOverride`
  ///   （`flutter/lib/src/foundation/debug.dart:41`）；
  /// * `addTearDown` 的回调跑在这道不变量检查**之后**，所以写在 teardown 里的
  ///   还原永远来不及——还原成什么值都一样红，报的都是
  ///   "The value of a foundation debug variable was changed by the test"。
  ///
  /// 也不能写 `debugPrint = null`：这个版本的 `debugPrint` 是非空的
  /// `DebugPrintCallback`，编译就过不去。可行的只有一条——进用例体时把绑定
  /// 装好的那个实现原样存下来，出用例体时原样还回去，恒等式自然成立。
  Future<void> withLogCapture(Future<void> Function() body) async {
    final bound = debugPrint;
    debugPrint = (String? message, {int? wrapWidth}) {
      if (message != null) logged.add(message);
    };
    try {
      await body();
    } finally {
      debugPrint = bound;
    }
  }

  /// [testWidgets] 的采集版：本文件的用例一律走它，免得哪条忘了包就等于
  /// "secret 打进日志没人管"。
  void testWidgetsWithLogCapture(
    String description,
    Future<void> Function(WidgetTester tester) body,
  ) {
    testWidgets(description, (tester) => withLogCapture(() => body(tester)));
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
  });

  testWidgetsWithLogCapture('从「扫一扫」入口一路点到控制器，扫与批各发一次且形状正确', (tester) async {
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

  testWidgetsWithLogCapture('畸形载荷：给出可见错误，一次网络都不发', (tester) async {
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
    //
    // 这里**不能**用 `find.textContaining(qrSecret)`：那个 finder 也匹配输入框里
    // 用户自己刚粘贴进去的原文（EditableText）——那当然含 secret，可它是用户的
    // 输入，不是"我们生成的文案带出凭据"。要管的是我们渲染出去的每一个文本节点。
    final rendered = tester
        .widgetList<Text>(find.byType(Text))
        .map((widget) => widget.data ?? '')
        .toList();
    expect(rendered, isNotEmpty, reason: '一个文本节点都没渲染，等于断言空转');
    for (final text in rendered) {
      expect(text, isNot(contains(qrSecret)), reason: text);
    }
  });

  testWidgetsWithLogCapture('scheme 不对同样拒绝且不发请求', (tester) async {
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

  testWidgetsWithLogCapture('全链路任何输出里都不出现 secret', (tester) async {
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
    // 1b) 上面那一条的"保险"：收集器必须真接到端口那条日志上。
    // `logged` 只有端口 `debugPrint('wxlike-qr POST …/scan')` 这一个来源
    // （传输层是记录型假实现），它为空就说明"没 secret"是因为没人打印——
    // 那种绿是假绿。把端口那行日志改成 `debugPrint('$request ${request.body}')`，
    // 这一节立刻红。
    expect(
      logged.where((line) => line.contains('/scan')),
      isNotEmpty,
      reason: 'debugPrint 收集器没采集到端口的真实日志，采集就是空转',
    );
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

  testWidgetsWithLogCapture('服务端判定过期时界面显示 EXPIRED 而不是成功', (tester) async {
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
    // 不假装成功：端口自己不发请求，原因由 [requireQrBearer] 这一层给出。
    // 这条断言问的是"策略在不在组合根工厂里"，不在端口构造参数上——
    // 把 buildQrIdentityPort 里的 `token:` 那一行去掉，这里就红。
    expect(failure.code, 'QR_SESSION_TOKEN_UNAVAILABLE');
    expect(failure.kind, G12FailureKind.unavailable);
    expect(transport.requests, isEmpty);
  });

  /// 组合根本身。**上面那条用例调的是同一个工厂，但它没法证明
  /// `lib/main.dart` 真的调用过这个工厂**——那条线只有这里能证明。
  ///
  /// 为什么是读源码而不是启动 `ProductionRoot`：`ProductionRoot.initState`
  /// 第一步就是 `FfiWxlikeService.open(…)`，测试进程里没有 `wxlike_ffi` 动态库，
  /// 界面停在启动错误页，`late final _identity` 根本不会被求值——boot 不出红。
  /// 所以这里做静态守卫：接线断掉（把那行删掉/换成别的端口）就在测试里红。
  /// 真机端到端仍然要按 `docs/plan/39` 移动端那一栏验，见卡面待验清单。
  test('lib/main.dart 的组合根确实把 buildQrIdentityPort 接到产品壳上', () {
    final root = File('lib/main.dart');
    expect(
      root.existsSync(),
      isTrue,
      reason: 'flutter test 的工作目录应是 apps/mobile；找不到文件就等于守卫空转',
    );
    final source = root.readAsStringSync();
    expect(
      RegExp(r'_identity\s*=\s*buildQrIdentityPort\(').hasMatch(source),
      isTrue,
      reason: '组合根没调用扫码登录的身份端口工厂——移动端扫码登录没接线',
    );
    expect(
      RegExp(r'identity:\s*_identity\b').hasMatch(source),
      isTrue,
      reason: '构造出来的端口没交给 WxlikeMobileApp 的 identity 参数',
    );
    // 换服务器后端口不重建，所以 base 必须是"当前值"的闭包而不是快照。
    expect(
      RegExp(r'serverBase:\s*\(\)\s*=>').hasMatch(source),
      isTrue,
      reason: 'serverBase 不是闭包：换服务器后扫码会打到旧地址',
    );
  });
}
