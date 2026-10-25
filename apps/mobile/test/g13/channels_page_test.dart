import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:wxlike_mobile/features/content/g13/core.dart';
import 'package:wxlike_mobile/features/content/g13/models.dart';
import 'package:wxlike_mobile/features/content/g13/unavailable_ports.dart';
import 'package:wxlike_mobile/features/video/channels_page.dart';

import 'g13_support.dart';

class _FakeVideo extends UnavailableVideoPort {
  int stops = 0;

  @override
  Future<PageResult<VideoItem>> feed({
    String cursor = '',
    int limit = 20,
  }) async {
    return const PageResult<VideoItem>(
      items: [
        VideoItem(
          id: 'v-1',
          author: 'Alice',
          title: '可播放',
          durationMs: 12000,
          coverColor: 0xff223344,
          playable: true,
        ),
        VideoItem(
          id: 'v-2',
          author: 'Bob',
          title: '转码中的视频',
          durationMs: 8000,
          coverColor: 0xff445566,
          playable: false,
        ),
      ],
      hasMore: false,
      snapshotVersion: 1,
    );
  }

  @override
  Future<String> playUrl(String videoId) async => 'stream://$videoId';

  @override
  Future<void> stopPlayback() async {
    stops++;
  }
}

Future<void> _open(WidgetTester tester, _FakeVideo fake) async {
  await tester.pumpWidget(
    MaterialApp(
      home: ChannelsPage(ports: testPorts(video: fake)),
    ),
  );
  await tester.pumpAndSettle();
}

void main() {
  testWidgets('不可播放条目不可点开', (tester) async {
    final fake = _FakeVideo();
    await _open(tester, fake);

    expect(find.textContaining('转码中/已下架'), findsOneWidget);
    await tester.tap(find.text('转码中的视频'));
    await tester.pumpAndSettle();
    expect(find.text('视频号'), findsOneWidget);
    expect(find.text('可播放'), findsOneWidget);
  });

  testWidgets('播放页无平台桥时显示阻塞，离页释放播放器', (tester) async {
    final fake = _FakeVideo();
    await _open(tester, fake);

    await tester.tap(find.text('可播放'));
    await tester.pumpAndSettle();
    expect(find.textContaining('播放器未接入'), findsOneWidget);

    await tester.tap(find.byIcon(Icons.arrow_back));
    await tester.pumpAndSettle();
    expect(fake.stops, 1);
  });
}
