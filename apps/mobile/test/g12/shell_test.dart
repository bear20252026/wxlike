import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:wxlike_mobile/shell/app_lock.dart';
import 'package:wxlike_mobile/shell/app_shell.dart';
import 'package:wxlike_mobile/platform/platform_ports.dart';
import 'package:wxlike_mobile/shell/deep_link.dart';
import 'package:wxlike_mobile/shell/theme.dart';

import 'support.dart';

void main() {
  group('DeepLink', () {
    test('识别会话、扫码与通讯录，其余明确拒绝', () {
      final chat = DeepLink.parse(Uri.parse('wxlike://chat/d-alice~bob'));
      expect(chat.kind, DeepLinkKind.chat);
      expect(chat.conv, 'd-alice~bob');

      final qr = DeepLink.parse(Uri.parse('wxlike://qr/challenge-1'));
      expect(qr.kind, DeepLinkKind.qrApproval);
      expect(qr.challengeId, 'challenge-1');

      expect(
        DeepLink.parse(Uri.parse('wxlike://contacts')).kind,
        DeepLinkKind.contacts,
      );
      expect(
        DeepLink.parse(Uri.parse('https://example.com/chat/x')).kind,
        DeepLinkKind.unknown,
      );
      expect(
        DeepLink.parse(Uri.parse('wxlike://chat')).kind,
        DeepLinkKind.unknown,
      );
      expect(
        DeepLink.parse(Uri.parse('wxlike://qr/a/b')).kind,
        DeepLinkKind.unknown,
      );
    });
  });

  group('AppLockController', () {
    test('冷启动恢复 PIN 后直接上锁，错误 PIN 不解锁', () async {
      final secrets = MemorySecrets()
        ..values[AppLockController.pinKey] = '1234';
      final controller = AppLockController(secrets: secrets);
      await controller.restore();

      expect(controller.enabled, isTrue);
      expect(controller.locked, isTrue);

      expect(await controller.unlock('0000'), isFalse);
      expect(controller.locked, isTrue);
      expect(controller.error, 'PIN 不正确');

      expect(await controller.unlock('1234'), isTrue);
      expect(controller.locked, isFalse);
    });

    test('后台超过宽限期才重新上锁', () async {
      var now = DateTime.utc(2026, 10, 24, 8);
      final secrets = MemorySecrets()
        ..values[AppLockController.pinKey] = '1234';
      final controller = AppLockController(
        secrets: secrets,
        grace: const Duration(seconds: 60),
        now: () => now,
      );
      await controller.restore();
      await controller.unlock('1234');

      controller.onBackground();
      now = now.add(const Duration(seconds: 30));
      controller.onForeground();
      expect(controller.locked, isFalse);

      controller.onBackground();
      now = now.add(const Duration(seconds: 61));
      controller.onForeground();
      expect(controller.locked, isTrue);
    });

    test('PIN 校验与清除都必须先解锁', () async {
      final secrets = MemorySecrets();
      final controller = AppLockController(secrets: secrets);
      await controller.restore();

      expect(await controller.setPin('12'), isFalse);
      expect(controller.error, 'PIN 需要 4–6 位数字');

      expect(await controller.setPin('4321'), isTrue);
      expect(controller.enabled, isTrue);
      expect(controller.locked, isFalse);

      expect(await controller.disable('0000'), isFalse);
      expect(controller.enabled, isTrue);

      expect(await controller.disable('4321'), isTrue);
      expect(controller.enabled, isFalse);
      expect(secrets.values.containsKey(AppLockController.pinKey), isFalse);
    });
  });

  group('AppShell', () {
    late AppLockController lock;
    late FakeLifecycle lifecycle;

    setUp(() async {
      lock = AppLockController(secrets: MemorySecrets());
      await lock.restore();
      lifecycle = FakeLifecycle();
    });

    tearDown(() => lifecycle.close());

    Widget buildShell({
      GlobalKey<AppShellState>? key,
      ValueChanged<String>? onChat,
    }) {
      return MaterialApp(
        theme: WxTheme.light(),
        home: AppShell(
          key: key,
          lock: lock,
          lifecycle: lifecycle,
          onSignOut: () async {},
          chatPageBuilder: (context, conv) => Scaffold(body: Text('聊天 $conv')),
          qrApprovalPageBuilder: (context, id) =>
              Scaffold(body: Text('扫码 $id')),
          tabs: [
            ShellTabSpec(
              label: '微信',
              icon: Icons.chat_bubble_outline,
              selectedIcon: Icons.chat_bubble,
              builder: (context) => const Text('会话页'),
            ),
            ShellTabSpec(
              label: '通讯录',
              icon: Icons.contacts_outlined,
              selectedIcon: Icons.contacts,
              builder: (context) => const Text('通讯录页'),
            ),
            ShellTabSpec(
              label: '我',
              icon: Icons.person_outline,
              selectedIcon: Icons.person,
              builder: (context) => const Text('我的页面'),
            ),
          ],
        ),
      );
    }

    testWidgets('底部栏切换、深链与未知链接', (tester) async {
      final key = GlobalKey<AppShellState>();
      await tester.pumpWidget(buildShell(key: key));

      expect(find.text('会话页'), findsOneWidget);
      await tester.tap(find.text('通讯录'));
      await tester.pumpAndSettle();
      expect(key.currentState!.currentTab, 1);
      expect(find.text('通讯录页'), findsOneWidget);

      await key.currentState!.handleDeepLink(Uri.parse('wxlike://chat/c1'));
      await tester.pumpAndSettle();
      expect(find.text('聊天 c1'), findsOneWidget);
      Navigator.of(tester.element(find.text('聊天 c1'))).pop();
      await tester.pumpAndSettle();

      await key.currentState!.handleDeepLink(Uri.parse('wxlike://qr/ch-1'));
      await tester.pumpAndSettle();
      expect(find.text('扫码 ch-1'), findsOneWidget);
      Navigator.of(tester.element(find.text('扫码 ch-1'))).pop();
      await tester.pumpAndSettle();

      await key.currentState!.handleDeepLink(
        Uri.parse('https://bad.example/x'),
      );
      await tester.pump();
      expect(find.textContaining('无法识别的链接'), findsOneWidget);
    });

    testWidgets('上锁时覆盖产品壳，输入正确 PIN 才恢复', (tester) async {
      final secrets = MemorySecrets()
        ..values[AppLockController.pinKey] = '1234';
      lock = AppLockController(secrets: secrets);
      await lock.restore();
      await tester.pumpWidget(buildShell());

      expect(find.text('应用已锁定'), findsOneWidget);

      await tester.enterText(find.byKey(const Key('app-lock-pin')), '0000');
      await tester.tap(find.text('解锁'));
      await tester.pumpAndSettle();
      expect(find.text('应用已锁定'), findsOneWidget);

      await tester.enterText(find.byKey(const Key('app-lock-pin')), '1234');
      await tester.tap(find.text('解锁'));
      await tester.pumpAndSettle();
      expect(find.text('应用已锁定'), findsNothing);
      expect(find.text('会话页'), findsOneWidget);
    });

    testWidgets('切后台超过宽限期后重新上锁', (tester) async {
      final secrets = MemorySecrets()
        ..values[AppLockController.pinKey] = '1234';
      lock = AppLockController(secrets: secrets, grace: Duration.zero);
      await lock.restore();
      await lock.unlock('1234');
      await tester.pumpWidget(buildShell());
      expect(find.text('应用已锁定'), findsNothing);

      lifecycle.emit(AppLifecycleSignal.paused);
      await tester.pump();
      lifecycle.emit(AppLifecycleSignal.resumed);
      await tester.pumpAndSettle();
      expect(find.text('应用已锁定'), findsOneWidget);
    });
  });
}
