import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:wxlike_mobile/features/content/g13/core.dart';
import 'package:wxlike_mobile/features/content/g13/models.dart';
import 'package:wxlike_mobile/features/content/g13/unavailable_ports.dart';
import 'package:wxlike_mobile/features/content/moments_page.dart';

import 'g13_support.dart';

class _FakeMoments extends UnavailableContentPort {
  _FakeMoments({this.mine = false, PageResult<Moment>? feed}) {
    this.feed = feed ?? _page(mine);
  }

  bool mine;
  late PageResult<Moment> feed;
  Object? likeError;
  Completer<void>? gate;
  int likes = 0;
  String? lastLikeKey;

  @override
  Future<PageResult<Moment>> moments({
    String cursor = '',
    int limit = 20,
  }) async => feed;

  @override
  Future<Moment> setMomentLike(
    String momentId,
    bool liked, {
    required String idempotencyKey,
  }) async {
    likes++;
    lastLikeKey = idempotencyKey;
    if (gate != null) await gate!.future;
    if (likeError != null) throw likeError!;
    final updated = feed.items.first.copyWith(
      liked: liked,
      likeCount: liked ? 1 : 0,
    );
    feed = PageResult<Moment>(
      items: [updated],
      hasMore: false,
      snapshotVersion: feed.snapshotVersion + 1,
    );
    return updated;
  }
}

PageResult<Moment> _page(bool mine) => PageResult<Moment>(
  items: [
    Moment(
      id: 'm-1',
      authorName: 'Alice',
      text: '今天天气不错',
      createdAt: DateTime.utc(2026, 1, 1),
      media: const [],
      liked: false,
      likeCount: 0,
      comments: const [],
      audience: 'all',
      isMine: mine,
    ),
  ],
  hasMore: false,
  snapshotVersion: 1,
);

Future<void> _open(WidgetTester tester, _FakeMoments fake) async {
  await tester.pumpWidget(
    MaterialApp(
      home: MomentsPage(ports: testPorts(content: fake)),
    ),
  );
  await tester.pumpAndSettle();
}

void main() {
  testWidgets('空动态：明确空态而不是空白页', (tester) async {
    final fake = _FakeMoments(
      feed: const PageResult<Moment>(
        items: [],
        hasMore: false,
        snapshotVersion: 1,
      ),
    );
    await _open(tester, fake);
    expect(find.text('还没有动态'), findsOneWidget);
  });

  testWidgets('点赞：带确定性幂等键，刷新后显示服务端计数', (tester) async {
    final fake = _FakeMoments();
    await _open(tester, fake);

    await tester.tap(find.widgetWithText(TextButton, '0'));
    await tester.pumpAndSettle();

    expect(fake.likes, 1);
    expect(fake.lastLikeKey, 'like-m-1-true');
    expect(find.widgetWithText(TextButton, '1'), findsOneWidget);
  });

  testWidgets('点赞未返回时重复点击只发一次', (tester) async {
    final gate = Completer<void>();
    final fake = _FakeMoments()..gate = gate;
    await _open(tester, fake);

    await tester.tap(find.byIcon(Icons.favorite_border));
    await tester.pump();
    await tester.tap(find.byIcon(Icons.favorite_border));
    await tester.pump();

    expect(fake.likes, 1);
    gate.complete();
    await tester.pumpAndSettle();
  });

  testWidgets('未授权：点赞失败提示重新登录', (tester) async {
    final fake = _FakeMoments()
      ..likeError = const G13Failure(
        G13FailureKind.unauthorized,
        'UNAUTHORIZED',
        '未登录',
      );
    await _open(tester, fake);

    await tester.tap(find.byIcon(Icons.favorite_border));
    await tester.pumpAndSettle();

    expect(find.text('登录已失效，请重新登录'), findsOneWidget);
  });

  testWidgets('删除入口只对本人动态出现', (tester) async {
    final other = _FakeMoments();
    await _open(tester, other);
    expect(find.byType(PopupMenuButton<String>), findsNothing);

    await tester.pumpWidget(const SizedBox());
    await tester.pump();

    final mine = _FakeMoments(mine: true);
    await tester.pumpWidget(
      MaterialApp(
        home: MomentsPage(ports: testPorts(content: mine)),
      ),
    );
    await tester.pumpAndSettle();
    expect(find.byType(PopupMenuButton<String>), findsOneWidget);
  });
}
