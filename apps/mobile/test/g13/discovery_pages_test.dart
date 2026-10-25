import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:wxlike_mobile/features/content/g13/core.dart';
import 'package:wxlike_mobile/features/content/g13/models.dart';
import 'package:wxlike_mobile/features/content/g13/unavailable_ports.dart';
import 'package:wxlike_mobile/features/discover/nearby_page.dart';
import 'package:wxlike_mobile/features/discover/recognition_page.dart';
import 'package:wxlike_mobile/features/discover/scan_page.dart';
import 'package:wxlike_mobile/features/mini/mini_programs_page.dart';

import 'g13_support.dart';

class _FakeDiscovery extends UnavailableDiscoveryPort {
  ScanResult? scan;
  Object? scanError;
  RecognitionResult? translation;
  Object? translateError;
  PageResult<NearbyPlace> nearbyPage = const PageResult<NearbyPlace>(
    items: [
      NearbyPlace(id: 'p-1', name: '西湖', distanceMeters: 120, cell: 'grid-1'),
    ],
    hasMore: false,
    snapshotVersion: 1,
  );
  int decodes = 0;
  int translates = 0;
  int nearbies = 0;

  @override
  Future<ScanResult> decodeScan(String imageRef) async {
    decodes++;
    if (scanError != null) throw scanError!;
    return scan!;
  }

  @override
  Future<RecognitionResult> translate(
    String text, {
    required String targetLanguage,
  }) async {
    translates++;
    if (translateError != null) throw translateError!;
    return translation!;
  }

  @override
  Future<PageResult<NearbyPlace>> nearby({
    required String cell,
    String cursor = '',
    int limit = 20,
  }) async {
    nearbies++;
    return nearbyPage;
  }
}

class _FakeMini extends UnavailableMiniPort {
  PageResult<MiniAppInfo>? recentPage;
  int launches = 0;

  @override
  Future<PageResult<MiniAppInfo>> recent({
    String cursor = '',
    int limit = 20,
  }) async => recentPage ?? PageResult.empty<MiniAppInfo>();

  @override
  Future<OperationHandle> launch(
    String appId, {
    required String idempotencyKey,
  }) async {
    launches++;
    return const OperationHandle(
      operationId: 'op-1',
      state: 'RUNNING',
      progress: 0.2,
      canCancel: true,
    );
  }
}

Future<void> _openRecognition(WidgetTester tester, _FakeDiscovery fake) async {
  tester.view.physicalSize = const Size(800, 1400);
  tester.view.devicePixelRatio = 1;
  addTearDown(tester.view.resetPhysicalSize);
  addTearDown(tester.view.resetDevicePixelRatio);
  await tester.pumpWidget(
    MaterialApp(
      home: RecognitionPage(ports: testPorts(discovery: fake)),
    ),
  );
  await tester.pumpAndSettle();
}

void main() {
  testWidgets('扫码：不安全目标只展示原因，不跳转', (tester) async {
    final fake = _FakeDiscovery()
      ..scan = const ScanResult(
        kind: 'payment',
        raw: 'https://evil.example/pay',
        targetId: 'pay-1',
        safe: false,
        reason: '域名不在白名单',
      );
    await tester.pumpWidget(
      MaterialApp(
        home: ScanPage(
          ports: testPorts(discovery: fake),
          capture: () async => 'frame-1',
        ),
      ),
    );
    await tester.tap(find.text('开始扫描'));
    await tester.pumpAndSettle();

    expect(fake.decodes, 1);
    expect(find.textContaining('未通过安全校验'), findsOneWidget);
    expect(find.text('收付款'), findsNothing);
  });

  testWidgets('扫码：未授权显示重新登录', (tester) async {
    final fake = _FakeDiscovery()
      ..scanError = const G13Failure(
        G13FailureKind.unauthorized,
        'UNAUTHORIZED',
        '未登录',
      );
    await tester.pumpWidget(
      MaterialApp(
        home: ScanPage(
          ports: testPorts(discovery: fake),
          capture: () async => 'frame-1',
        ),
      ),
    );
    await tester.tap(find.text('开始扫描'));
    await tester.pumpAndSettle();

    expect(find.text('登录已失效，请重新登录'), findsOneWidget);
  });

  testWidgets('识物：相机未接入时明确阻塞', (tester) async {
    final fake = _FakeDiscovery();
    await _openRecognition(tester, fake);

    await tester.tap(find.text('选择图片（未接入）'));
    await tester.pumpAndSettle();

    expect(find.textContaining('相机与图片解码由平台桥提供'), findsOneWidget);
    expect(fake.decodes, 0);
  });

  testWidgets('翻译：空文本是参数错误，不发起请求', (tester) async {
    final fake = _FakeDiscovery();
    await _openRecognition(tester, fake);

    await tester.tap(find.text('翻译'));
    await tester.pumpAndSettle();

    expect(find.text('请输入要翻译的内容'), findsOneWidget);
    expect(fake.translates, 0);
  });

  testWidgets('翻译：结果与目标语言来自服务端', (tester) async {
    final fake = _FakeDiscovery()
      ..translation = const RecognitionResult(
        label: '',
        confidence: 0,
        translation: 'Hello',
        sourceText: '你好',
        targetLanguage: 'en',
      );
    await _openRecognition(tester, fake);

    await tester.enterText(find.byType(TextField), '你好');
    await tester.tap(find.text('翻译'));
    await tester.pumpAndSettle();

    expect(fake.translates, 1);
    expect(find.text('Hello'), findsOneWidget);
    expect(find.text('翻译结果'), findsOneWidget);
    expect(find.text('目标语言：en'), findsOneWidget);
  });

  testWidgets('附近：定位撤权后停止查询并说明原因', (tester) async {
    final fake = _FakeDiscovery();
    await tester.pumpWidget(
      MaterialApp(
        home: NearbyPage(
          ports: testPorts(discovery: fake),
          cellProvider: () async => null,
        ),
      ),
    );
    await tester.pumpAndSettle();

    expect(find.text('定位权限未开启，撤回授权后不再查询附近'), findsOneWidget);
    expect(fake.nearbies, 0);
  });

  testWidgets('附近：使用粗粒度网格查询并展示结果', (tester) async {
    final fake = _FakeDiscovery();
    await tester.pumpWidget(
      MaterialApp(
        home: NearbyPage(
          ports: testPorts(discovery: fake),
          cellProvider: () async => 'grid-1',
        ),
      ),
    );
    await tester.pumpAndSettle();

    expect(fake.nearbies, 1);
    expect(find.text('西湖'), findsOneWidget);
    expect(find.textContaining('网格 grid-1'), findsWidgets);
  });

  testWidgets('小程序：撤销版本不提供启动入口', (tester) async {
    final fake = _FakeMini()
      ..recentPage = const PageResult<MiniAppInfo>(
        items: [
          MiniAppInfo(
            appId: 'app-1',
            name: '示例小程序',
            version: '1.0.3',
            installed: true,
            recent: true,
            revoked: true,
            scopes: [],
          ),
        ],
        hasMore: false,
        snapshotVersion: 2,
      );
    await tester.pumpWidget(
      MaterialApp(
        home: MiniProgramsPage(ports: testPorts(mini: fake)),
      ),
    );
    await tester.pumpAndSettle();

    expect(find.text('版本已撤销，无法启动'), findsOneWidget);
    expect(find.text('打开'), findsNothing);
    await tester.tap(find.text('示例小程序'));
    await tester.pumpAndSettle();
    expect(fake.launches, 0);
  });
}
