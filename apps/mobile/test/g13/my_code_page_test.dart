import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:wxlike_mobile/features/content/g13/models.dart';
import 'package:wxlike_mobile/features/content/g13/ports.dart';
import 'package:wxlike_mobile/features/discover/mycode/my_code_page.dart';

import 'g13_support.dart';

class _FakeQr implements QrPort {
  int saves = 0;
  int loads = 0;
  Completer<void>? gate;
  Object? saveError;
  String? lastSaveKey;
  QrStyle? lastStyle;
  bool scannable = true;

  MyQrCode _code(QrStyle style) => MyQrCode(
    payload: 'https://wxlike.example/q/${style.name}',
    style: style,
    version: 3,
    expiresAt: DateTime.utc(2027, 1, 1),
    scannable: scannable,
  );

  @override
  Future<MyQrCode> myCode(
    QrStyle style, {
    required String idempotencyKey,
  }) async {
    loads++;
    lastStyle = style;
    return _code(style);
  }

  @override
  Future<MyQrCode> rotate({required String idempotencyKey}) async =>
      _code(QrStyle.ink);

  @override
  Future<void> saveToAlbum(
    String payload, {
    required String idempotencyKey,
  }) async {
    saves++;
    lastSaveKey = idempotencyKey;
    if (gate != null) await gate!.future;
    if (saveError != null) throw saveError!;
  }
}

Future<void> _open(WidgetTester tester, _FakeQr fake) async {
  await tester.pumpWidget(
    MaterialApp(
      home: MyCodePage(ports: testPorts(qr: fake)),
    ),
  );
  await tester.pumpAndSettle();
}

void main() {
  testWidgets('渲染未接入：明确阻塞，不画假二维码；payload 仍展示', (tester) async {
    final fake = _FakeQr();
    await _open(tester, fake);

    expect(find.textContaining('二维码渲染由平台桥提供'), findsOneWidget);
    expect(
      find.textContaining('https://wxlike.example/q/spring'),
      findsOneWidget,
    );
  });

  testWidgets('样式切换：向端口请求对应样式', (tester) async {
    final fake = _FakeQr();
    await _open(tester, fake);

    await tester.tap(find.widgetWithText(ChoiceChip, '墨绿'));
    await tester.pumpAndSettle();

    expect(fake.loads, 2);
    expect(fake.lastStyle, QrStyle.ink);
  });

  testWidgets('不可扫：按服务端 scannable=false 提示，不宣称可扫', (tester) async {
    final fake = _FakeQr()..scannable = false;
    await _open(tester, fake);

    expect(find.text('服务端未确认该码可扫码'), findsOneWidget);
  });

  testWidgets('防重：保存未返回时重复点击只写一次相册', (tester) async {
    final gate = Completer<void>();
    final fake = _FakeQr()..gate = gate;
    await _open(tester, fake);

    await tester.tap(find.text('保存到相册'));
    await tester.pump();
    await tester.tap(find.text('保存中…'));
    await tester.pump();

    expect(fake.saves, 1);
    expect(fake.lastSaveKey, isNotEmpty);

    gate.complete();
    await tester.pumpAndSettle();
  });

  testWidgets('保存失败：显示失败原因', (tester) async {
    final fake = _FakeQr()..saveError = Exception('相册不可写');
    await _open(tester, fake);

    await tester.tap(find.text('保存到相册'));
    await tester.pumpAndSettle();

    expect(find.textContaining('相册不可写'), findsOneWidget);
    expect(fake.saves, 1);
  });
}
