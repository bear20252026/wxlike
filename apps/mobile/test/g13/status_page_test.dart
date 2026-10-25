import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:wxlike_mobile/features/content/g13/core.dart';
import 'package:wxlike_mobile/features/content/g13/ports.dart';
import 'package:wxlike_mobile/features/content/g13/models.dart';
import 'package:wxlike_mobile/features/content/status/status_page.dart';

import 'g13_support.dart';

class _FakeStatus implements StatusPort {
  UserStatus? mine;
  PageResult<UserStatus>? friends;
  Object? publishError;
  Completer<void>? gate;
  int publishCalls = 0;
  String? lastKey;
  String? lastVisibility;
  String? lastText;

  @override
  Future<UserStatus?> myStatus() async => mine;

  @override
  Future<PageResult<UserStatus>> friendStatuses({
    String cursor = '',
    int limit = 20,
  }) async => friends ?? PageResult.empty<UserStatus>();

  @override
  Future<UserStatus> publishStatus({
    required String text,
    required String background,
    required String visibility,
    required Duration ttl,
    required String idempotencyKey,
  }) async {
    publishCalls++;
    lastKey = idempotencyKey;
    lastVisibility = visibility;
    lastText = text;
    if (gate != null) await gate!.future;
    if (publishError != null) throw publishError!;
    return UserStatus(
      id: 's-1',
      userId: 'u-1',
      displayName: 'Alice',
      text: text,
      background: background,
      expiresAt: DateTime.now().add(ttl),
      visibility: visibility,
    );
  }

  @override
  Future<void> clearStatus({required String idempotencyKey}) async {
    mine = null;
  }
}

Future<void> _open(WidgetTester tester, _FakeStatus fake) async {
  await tester.pumpWidget(
    MaterialApp(
      home: StatusPage(ports: testPorts(status: fake)),
    ),
  );
  await tester.pumpAndSettle();
}

void main() {
  testWidgets('空状态：我的与好友分别给出空文案', (tester) async {
    await _open(tester, _FakeStatus());

    expect(find.text('还没有发布状态'), findsOneWidget);
    expect(find.text('好友还没有发布状态'), findsOneWidget);
  });

  testWidgets('参数错误：空内容不发起发布', (tester) async {
    final fake = _FakeStatus();
    await _open(tester, fake);

    await tester.tap(find.byIcon(Icons.add_circle_outline));
    await tester.pumpAndSettle();
    await tester.tap(find.widgetWithText(FilledButton, '发布'));
    await tester.pumpAndSettle();

    expect(find.text('状态内容不能为空'), findsOneWidget);
    expect(fake.publishCalls, 0);
  });

  testWidgets('成功：发布带可见范围，返回后展示服务端状态', (tester) async {
    final fake = _FakeStatus();
    await _open(tester, fake);

    await tester.tap(find.byIcon(Icons.add_circle_outline));
    await tester.pumpAndSettle();
    await tester.enterText(find.byType(TextField), '旅行中');
    await tester.tap(find.widgetWithText(ChoiceChip, '仅自己'));
    await tester.pump();
    await tester.tap(find.widgetWithText(FilledButton, '发布'));
    await tester.pumpAndSettle();

    expect(fake.publishCalls, 1);
    expect(fake.lastText, '旅行中');
    expect(fake.lastVisibility, 'private');
    expect(fake.lastKey, isNotEmpty);
    expect(find.text('旅行中'), findsOneWidget);
  });

  testWidgets('防重：发布未返回时重复点击只发一次', (tester) async {
    final gate = Completer<void>();
    final fake = _FakeStatus()..gate = gate;
    await _open(tester, fake);

    await tester.tap(find.byIcon(Icons.add_circle_outline));
    await tester.pumpAndSettle();
    await tester.enterText(find.byType(TextField), '旅行中');
    await tester.tap(find.widgetWithText(FilledButton, '发布'));
    await tester.pump();
    await tester.tap(find.byType(FilledButton).last);
    await tester.pump();

    expect(fake.publishCalls, 1);
    gate.complete();
    await tester.pumpAndSettle();
  });
}
