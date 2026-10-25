import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:wxlike_mobile/features/content/g13/core.dart';
import 'package:wxlike_mobile/features/content/g13/models.dart';
import 'package:wxlike_mobile/features/content/g13/ports.dart';
import 'package:wxlike_mobile/features/settings/settings_page.dart';

import 'g13_support.dart';

class _FakePrefs implements PreferencesPort {
  _FakePrefs(this.current);

  Preferences current;
  Object? error;
  Completer<void>? gate;
  int updateCalls = 0;
  String? lastKey;
  Preferences? lastNext;

  @override
  Future<Preferences> load() async => current;

  @override
  Future<Preferences> update(
    Preferences next, {
    required String idempotencyKey,
  }) async {
    updateCalls++;
    lastKey = idempotencyKey;
    lastNext = next;
    if (gate != null) await gate!.future;
    if (error != null) throw error!;
    current = next;
    return next;
  }
}

Preferences _prefs() => const Preferences(
  messageNotifications: false,
  sound: false,
  vibration: false,
  findMeByPhone: false,
  walletLock: false,
  youthMode: false,
  darkMode: false,
  language: 'zh-Hans',
  version: 1,
);

Future<void> _open(
  WidgetTester tester,
  _FakePrefs fake, {
  LogoutAction? logout,
}) async {
  await tester.pumpWidget(
    MaterialApp(
      home: SettingsPage(
        ports: testPorts(preferences: fake),
        logout: logout,
      ),
    ),
  );
  await tester.pumpAndSettle();
}

void main() {
  testWidgets('成功：开关写服务端，界面以返回值生效', (tester) async {
    final fake = _FakePrefs(_prefs());
    await _open(tester, fake);

    await tester.tap(find.text('新消息通知'));
    await tester.pumpAndSettle();

    expect(fake.updateCalls, 1);
    expect(fake.lastNext!.messageNotifications, isTrue);
    expect(fake.lastKey, isNotEmpty);
    final tile = tester.widget<SwitchListTile>(
      find.widgetWithText(SwitchListTile, '新消息通知'),
    );
    expect(tile.value, isTrue);
  });

  testWidgets('防重：保存期间其它开关不可触发第二次写入', (tester) async {
    final gate = Completer<void>();
    final fake = _FakePrefs(_prefs())..gate = gate;
    await _open(tester, fake);

    await tester.tap(find.text('新消息通知'));
    await tester.pump();
    await tester.tap(find.text('声音'));
    await tester.pump();

    expect(fake.updateCalls, 1);

    gate.complete();
    await tester.pumpAndSettle();
  });

  testWidgets('超时：可重试且复用同一个幂等键', (tester) async {
    final fake = _FakePrefs(_prefs())
      ..error = const G13Failure(
        G13FailureKind.timeout,
        'TIMEOUT',
        '超时',
        retryable: true,
      );
    await _open(tester, fake);

    await tester.tap(find.text('新消息通知'));
    await tester.pumpAndSettle();
    expect(find.text('请求超时，可以重试'), findsOneWidget);
    final firstKey = fake.lastKey;

    await tester.tap(find.text('重试'));
    await tester.pumpAndSettle();
    expect(fake.updateCalls, 2);
    expect(fake.lastKey, firstKey);
  });

  testWidgets('未授权：不提供重试按钮', (tester) async {
    final fake = _FakePrefs(_prefs())
      ..error = const G13Failure(
        G13FailureKind.unauthorized,
        'UNAUTHORIZED',
        '未登录',
      );
    await _open(tester, fake);

    await tester.tap(find.text('新消息通知'));
    await tester.pumpAndSettle();

    expect(find.text('登录已失效，请重新登录'), findsOneWidget);
    expect(find.text('重试'), findsNothing);
  });

  testWidgets('退出登录：未注入动作时明确说明阻塞', (tester) async {
    final fake = _FakePrefs(_prefs());
    await _open(tester, fake);

    await tester.scrollUntilVisible(find.text('退出登录'), 400);
    await tester.pumpAndSettle();
    await tester.tap(find.text('退出登录'));
    await tester.pumpAndSettle();
    await tester.tap(find.widgetWithText(FilledButton, '退出'));
    await tester.pumpAndSettle();

    await tester.scrollUntilVisible(find.textContaining('组合根注入登录服务'), -400);
    expect(find.textContaining('组合根注入登录服务'), findsOneWidget);
  });

  testWidgets('退出登录：注入动作后只调一次', (tester) async {
    final fake = _FakePrefs(_prefs());
    var logouts = 0;
    await _open(tester, fake, logout: () async => logouts++);

    await tester.scrollUntilVisible(find.text('退出登录'), 400);
    await tester.pumpAndSettle();
    await tester.tap(find.text('退出登录'));
    await tester.pumpAndSettle();
    await tester.tap(find.widgetWithText(FilledButton, '退出'));
    await tester.pumpAndSettle();

    expect(logouts, 1);
  });
}
