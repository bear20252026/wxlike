import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:wxlike_mobile/features/content/g13/core.dart';
import 'package:wxlike_mobile/features/content/g13/models.dart';
import 'package:wxlike_mobile/features/content/g13/ports.dart';
import 'package:wxlike_mobile/features/discover/location/location_page.dart';

import 'g13_support.dart';

class _FakeLocation implements LocationPort {
  LocationShare? currentShare;
  List<LocationShare> active = const [];
  PageResult<NearbyPlace> placesResult = const PageResult<NearbyPlace>(
    items: [
      NearbyPlace(id: 'p-1', name: '西湖', distanceMeters: 300, cell: 'grid-1'),
    ],
    hasMore: false,
    snapshotVersion: 1,
  );
  int sends = 0;
  int starts = 0;
  int stops = 0;
  String? lastSendKey;
  Completer<void>? gate;

  LocationShare _share(String id, String name) => LocationShare(
    id: id,
    convId: 'c-1',
    placeName: name,
    address: '杭州',
    latitude: 30.25,
    longitude: 120.15,
    active: true,
    expiresAt: DateTime.utc(2027, 1, 1),
  );

  @override
  Future<LocationShare?> currentPlace(String convId) async => currentShare;

  @override
  Future<PageResult<NearbyPlace>> places(
    String query, {
    String cursor = '',
    int limit = 20,
  }) async => placesResult;

  @override
  Future<LocationShare> sendLocation(
    String convId,
    String placeId, {
    required String idempotencyKey,
  }) async {
    sends++;
    lastSendKey = idempotencyKey;
    if (gate != null) await gate!.future;
    return _share('share-sent', '西湖');
  }

  @override
  Future<LocationShare> startSharing(
    String convId, {
    required int minutes,
    required String idempotencyKey,
  }) async {
    starts++;
    return _share('share-live', '当前位置');
  }

  @override
  Future<void> stopSharing(
    String shareId, {
    required String idempotencyKey,
  }) async {
    stops++;
    if (currentShare?.id == shareId) currentShare = null;
    active = active.where((item) => item.id != shareId).toList();
  }

  @override
  Future<List<LocationShare>> activeShares(String convId) async => active;
}

class _Host extends StatelessWidget {
  const _Host({required this.ports});

  final G13Ports ports;

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      body: Center(
        child: TextButton(
          onPressed: () => Navigator.of(context).push(
            MaterialPageRoute<void>(
              builder: (_) => LocationPage(ports: ports, convId: 'c-1'),
            ),
          ),
          child: const Text('open'),
        ),
      ),
    );
  }
}

Future<void> _openHost(WidgetTester tester, _FakeLocation fake) async {
  await tester.pumpWidget(
    MaterialApp(
      home: _Host(ports: testPorts(location: fake)),
    ),
  );
  await tester.tap(find.text('open'));
  await tester.pumpAndSettle();
}

void main() {
  testWidgets('地图未接入：坐标就绪但显示平台桥阻塞占位', (tester) async {
    final fake = _FakeLocation()..currentShare = null;
    await _openHost(tester, fake);
    expect(find.text('还没有共享位置'), findsOneWidget);
  });

  testWidgets('发送位置成功：返回调用方会话页面', (tester) async {
    final fake = _FakeLocation();
    await _openHost(tester, fake);

    await tester.tap(find.text('西湖'));
    await tester.pumpAndSettle();
    await tester.tap(find.text('发送到当前会话'));
    await tester.pumpAndSettle();

    expect(fake.sends, 1);
    expect(fake.lastSendKey, isNotEmpty);
    expect(find.text('open'), findsOneWidget);
  });

  testWidgets('发送按钮防重：未返回时只发一次', (tester) async {
    final gate = Completer<void>();
    final fake = _FakeLocation()..gate = gate;
    await _openHost(tester, fake);

    await tester.tap(find.text('西湖'));
    await tester.pumpAndSettle();
    await tester.tap(find.text('发送到当前会话'));
    await tester.pump();
    await tester.tap(find.byType(FilledButton).last);
    await tester.pump();

    expect(fake.sends, 1);
    gate.complete();
    await tester.pumpAndSettle();
  });

  testWidgets('地图渲染器注入后显示真实地图区域', (tester) async {
    final fake = _FakeLocation()
      ..currentShare = LocationShare(
        id: 'share-1',
        convId: 'c-1',
        placeName: '西湖',
        address: '杭州',
        latitude: 30.25,
        longitude: 120.15,
        active: true,
        expiresAt: DateTime.utc(2027, 1, 1),
      );
    await tester.pumpWidget(
      MaterialApp(
        home: LocationPage(
          ports: testPorts(location: fake),
          convId: 'c-1',
          mapBuilder: (context, latitude, longitude) =>
              Text('map:$latitude,$longitude'),
        ),
      ),
    );
    await tester.pumpAndSettle();

    expect(find.text('map:30.25,120.15'), findsOneWidget);
    expect(find.textContaining('平台桥提供'), findsNothing);

    await tester.tap(find.text('停止'));
    await tester.pumpAndSettle();
    expect(fake.stops, 1);
    expect(find.text('还没有共享位置'), findsOneWidget);
  });
}
