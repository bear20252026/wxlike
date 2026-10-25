import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:wxlike_mobile/features/content/g13/core.dart';
import 'package:wxlike_mobile/features/content/g13/models.dart';
import 'package:wxlike_mobile/features/content/g13/ports.dart';
import 'package:wxlike_mobile/features/settings/profile_page.dart';

import 'g13_support.dart';

class _FakeProfile implements ProfilePort {
  _FakeProfile(this.current);

  ProfileInfo current;
  Object? updateError;
  Completer<void>? gate;
  int updateCalls = 0;
  int? lastExpectedVersion;
  String? lastKey;
  String? lastName;

  @override
  Future<ProfileInfo> profile() async => current;

  @override
  Future<ProfileInfo> updateProfile({
    required String name,
    required String signature,
    required int expectedVersion,
    required String idempotencyKey,
  }) async {
    updateCalls++;
    lastName = name;
    lastExpectedVersion = expectedVersion;
    lastKey = idempotencyKey;
    if (gate != null) await gate!.future;
    if (updateError != null) throw updateError!;
    current = ProfileInfo(
      userId: current.userId,
      name: name,
      signature: signature,
      region: current.region,
      avatarMediaId: current.avatarMediaId,
      version: current.version + 1,
    );
    return current;
  }

  @override
  Future<SecurityInfo> security() => throw UnimplementedError();

  @override
  Future<StorageUsage> storage() => throw UnimplementedError();

  @override
  Future<StorageUsage> clearCache({required String idempotencyKey}) =>
      throw UnimplementedError();

  @override
  Future<OperationHandle> exportBackup({required String idempotencyKey}) =>
      throw UnimplementedError();

  @override
  Future<BackupFile> backupResult(String operationId) =>
      throw UnimplementedError();

  @override
  Future<void> restoreBackup(
    String fileId, {
    required String verificationDigest,
    required String idempotencyKey,
  }) => throw UnimplementedError();
}

const ProfileInfo _profile = ProfileInfo(
  userId: 'u-1',
  name: 'Alice',
  signature: '早睡',
  region: '杭州',
  avatarMediaId: '',
  version: 7,
);

Future<void> _open(WidgetTester tester, _FakeProfile fake) async {
  await tester.pumpWidget(
    MaterialApp(
      home: ProfilePage(ports: testPorts(profile: fake)),
    ),
  );
  await tester.pumpAndSettle();
}

void main() {
  testWidgets('成功：更新带 expected_version，返回后展示新资料', (tester) async {
    final fake = _FakeProfile(_profile);
    await _open(tester, fake);

    await tester.tap(find.text('编辑资料'));
    await tester.pumpAndSettle();
    await tester.enterText(find.byType(TextField).first, 'Alice2');
    await tester.tap(find.text('保存'));
    await tester.pumpAndSettle();

    expect(fake.updateCalls, 1);
    expect(fake.lastExpectedVersion, 7);
    expect(fake.lastKey, isNotEmpty);
    expect(fake.lastName, 'Alice2');
    expect(find.text('Alice2'), findsWidgets);
  });

  testWidgets('参数错误：空昵称不发起更新', (tester) async {
    final fake = _FakeProfile(_profile);
    await _open(tester, fake);

    await tester.tap(find.text('编辑资料'));
    await tester.pumpAndSettle();
    await tester.enterText(find.byType(TextField).first, '');
    await tester.tap(find.text('保存'));
    await tester.pumpAndSettle();

    expect(find.text('昵称不能为空'), findsOneWidget);
    expect(fake.updateCalls, 0);
  });

  testWidgets('冲突：提示刷新资料而不是反复重试', (tester) async {
    final fake = _FakeProfile(_profile)
      ..updateError = const G13Failure(
        G13FailureKind.conflict,
        'VERSION_CONFLICT',
        '版本冲突',
      );
    await _open(tester, fake);

    await tester.tap(find.text('编辑资料'));
    await tester.pumpAndSettle();
    await tester.enterText(find.byType(TextField).first, 'Alice2');
    await tester.tap(find.text('保存'));
    await tester.pumpAndSettle();

    expect(find.text('数据已变化，请刷新后再试'), findsOneWidget);
    expect(find.text('刷新资料'), findsOneWidget);
  });

  testWidgets('防重：保存未返回时重复点击只发一次', (tester) async {
    final gate = Completer<void>();
    final fake = _FakeProfile(_profile)..gate = gate;
    await _open(tester, fake);

    await tester.tap(find.text('编辑资料'));
    await tester.pumpAndSettle();
    await tester.enterText(find.byType(TextField).first, 'Alice2');
    await tester.tap(find.text('保存'));
    await tester.pump();
    await tester.tap(find.byType(FilledButton).last);
    await tester.pump();

    expect(fake.updateCalls, 1);
    gate.complete();
    await tester.pumpAndSettle();
  });
}
