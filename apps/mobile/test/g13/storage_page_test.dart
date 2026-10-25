import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:wxlike_mobile/features/content/g13/core.dart';
import 'package:wxlike_mobile/features/content/g13/models.dart';
import 'package:wxlike_mobile/features/content/g13/unavailable_ports.dart';
import 'package:wxlike_mobile/features/settings/storage_page.dart';

import 'g13_support.dart';

class _FakeProfile extends UnavailableProfilePort {
  StorageUsage usage = const StorageUsage(
    cacheBytes: 1024,
    mediaBytes: 2 * 1024 * 1024,
    databaseBytes: 512,
  );
  int clears = 0;
  String? lastClearKey;
  Completer<void>? gate;
  int exports = 0;

  @override
  Future<StorageUsage> storage() async => usage;

  @override
  Future<StorageUsage> clearCache({required String idempotencyKey}) async {
    clears++;
    lastClearKey = idempotencyKey;
    if (gate != null) await gate!.future;
    usage = StorageUsage(
      cacheBytes: 0,
      mediaBytes: usage.mediaBytes,
      databaseBytes: usage.databaseBytes,
    );
    return usage;
  }

  @override
  Future<OperationHandle> exportBackup({required String idempotencyKey}) async {
    exports++;
    return const OperationHandle(
      operationId: 'op-1',
      state: 'RUNNING',
      progress: 0.4,
      canCancel: true,
    );
  }

  @override
  Future<BackupFile> backupResult(String operationId) async => BackupFile(
    fileId: 'f-1',
    createdAt: DateTime.utc(2026, 5, 1),
    sizeBytes: 4096,
    verified: true,
  );
}

Future<void> _open(WidgetTester tester, _FakeProfile fake) async {
  tester.view.physicalSize = const Size(800, 1400);
  tester.view.devicePixelRatio = 1;
  addTearDown(tester.view.resetPhysicalSize);
  addTearDown(tester.view.resetDevicePixelRatio);
  await tester.pumpWidget(
    MaterialApp(
      home: StoragePage(ports: testPorts(profile: fake)),
    ),
  );
  await tester.pumpAndSettle();
}

void main() {
  testWidgets('真实大小来自服务端并区分缓存/媒体/数据库', (tester) async {
    final fake = _FakeProfile();
    await _open(tester, fake);

    expect(find.text('1.0 KB'), findsOneWidget);
    expect(find.text('2.0 MB'), findsNWidgets(2));
    expect(find.text('512 B'), findsOneWidget);
  });

  testWidgets('清缓存：确认后带幂等键，结果以服务端返回为准', (tester) async {
    final fake = _FakeProfile();
    await _open(tester, fake);

    await tester.tap(find.widgetWithText(FilledButton, '清理缓存'));
    await tester.pumpAndSettle();
    await tester.tap(find.widgetWithText(FilledButton, '清理'));
    await tester.pumpAndSettle();

    expect(fake.clears, 1);
    expect(fake.lastClearKey, isNotEmpty);
    expect(find.text('0 B'), findsOneWidget);
  });

  testWidgets('清缓存防重：未返回时重复点击只发一次', (tester) async {
    final gate = Completer<void>();
    final fake = _FakeProfile()..gate = gate;
    await _open(tester, fake);

    await tester.tap(find.widgetWithText(FilledButton, '清理缓存'));
    await tester.pumpAndSettle();
    await tester.tap(find.widgetWithText(FilledButton, '清理'));
    await tester.pump(const Duration(milliseconds: 400));
    await tester.tap(find.byType(FilledButton).last, warnIfMissed: false);
    await tester.pump();

    expect(fake.clears, 1);
    gate.complete();
    await tester.pumpAndSettle();
  });

  testWidgets('备份：导出作业可查询，未校验前不宣称可用', (tester) async {
    final fake = _FakeProfile();
    await _open(tester, fake);

    await tester.tap(find.widgetWithText(FilledButton, '导出备份'));
    await tester.pumpAndSettle();
    expect(fake.exports, 1);
    expect(find.textContaining('RUNNING'), findsOneWidget);

    await tester.tap(find.text('查询备份文件'));
    await tester.pumpAndSettle();

    expect(find.text('备份文件已校验'), findsOneWidget);
    expect(find.textContaining('4.0 KB'), findsOneWidget);
    expect(find.text('可恢复'), findsOneWidget);
  });
}
