import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:wxlike_mobile/features/content/g13/core.dart';
import 'package:wxlike_mobile/features/content/g13/models.dart';
import 'package:wxlike_mobile/features/content/g13/unavailable_ports.dart';
import 'package:wxlike_mobile/features/content/official/official_page.dart';

import 'g13_support.dart';

class _FakePublisher extends UnavailablePublisherPort {
  _FakePublisher({PageResult<Publisher>? page}) : page = page ?? _page();

  PageResult<Publisher> page;
  String? lastQuery;
  int subs = 0;
  String? lastKey;
  int? lastExpectedVersion;
  final List<String> articleIds = [];
  final List<int> docVersions = [];

  @override
  Future<PageResult<Publisher>> publishers({
    String query = '',
    String cursor = '',
    int limit = 20,
  }) async {
    lastQuery = query;
    return page;
  }

  @override
  Future<Publisher> setSubscribed(
    String publisherId,
    bool subscribed, {
    required int expectedVersion,
    required String idempotencyKey,
  }) async {
    subs++;
    lastKey = idempotencyKey;
    lastExpectedVersion = expectedVersion;
    final updated = Publisher(
      id: publisherId,
      name: '设计手记',
      description: '关于设计',
      unread: 2,
      subscribed: subscribed,
      version: expectedVersion + 1,
    );
    page = PageResult<Publisher>(
      items: [updated],
      hasMore: false,
      snapshotVersion: page.snapshotVersion + 1,
    );
    return updated;
  }

  @override
  Future<PageResult<Article>> articles(
    String publisherId, {
    String cursor = '',
    int limit = 20,
  }) async {
    return PageResult<Article>(
      items: [_article('a-1', '第一篇', 3), _article('a-2', '第二篇', 4)],
      hasMore: false,
      snapshotVersion: 1,
    );
  }

  @override
  Future<Article> article(String articleId, {required int docVersion}) async {
    articleIds.add(articleId);
    docVersions.add(docVersion);
    return _article(articleId, articleId == 'a-1' ? '第一篇' : '第二篇', docVersion);
  }
}

PageResult<Publisher> _page() => const PageResult<Publisher>(
  items: [
    Publisher(
      id: 'pub-1',
      name: '设计手记',
      description: '关于设计',
      unread: 2,
      subscribed: false,
      version: 7,
    ),
  ],
  hasMore: false,
  snapshotVersion: 1,
);

Article _article(String id, String title, int docVersion) => Article(
  id: id,
  publisherId: 'pub-1',
  title: title,
  summary: '摘要',
  author: '作者',
  publishedAt: DateTime.utc(2026, 1, 1),
  docVersion: docVersion,
  blocks: const [ArticleBlock(kind: 'paragraph', text: '正文')],
  revoked: false,
);

Future<void> _open(WidgetTester tester, _FakePublisher fake) async {
  await tester.pumpWidget(
    MaterialApp(
      home: OfficialPage(ports: testPorts(publisher: fake)),
    ),
  );
  await tester.pumpAndSettle();
}

void main() {
  testWidgets('列表展示未读与添加；关注带 expected_version', (tester) async {
    final fake = _FakePublisher();
    await _open(tester, fake);

    expect(find.text('2 条未读'), findsOneWidget);
    await tester.tap(find.text('添加'));
    await tester.pumpAndSettle();

    expect(fake.subs, 1);
    expect(fake.lastExpectedVersion, 7);
    expect(fake.lastKey, isNotEmpty);

    await tester.tap(find.text('已关注'));
    await tester.pumpAndSettle();
    expect(fake.subs, 2);
  });

  testWidgets('搜索交给服务端，查询词原样传递', (tester) async {
    final fake = _FakePublisher();
    await _open(tester, fake);

    await tester.enterText(find.byType(TextField), '设计');
    await tester.testTextInput.receiveAction(TextInputAction.search);
    await tester.pumpAndSettle();

    expect(fake.lastQuery, '设计');
  });

  testWidgets('文章卡打开各自的 article_id 与 docVersion', (tester) async {
    final fake = _FakePublisher();
    await _open(tester, fake);

    await tester.tap(find.text('设计手记'));
    await tester.pumpAndSettle();
    await tester.tap(find.text('第一篇'));
    await tester.pumpAndSettle();

    expect(fake.articleIds, ['a-1']);
    expect(fake.docVersions, [3]);

    await tester.tap(find.byIcon(Icons.arrow_back));
    await tester.pumpAndSettle();
    await tester.tap(find.text('第二篇'));
    await tester.pumpAndSettle();

    expect(fake.articleIds, ['a-1', 'a-2']);
    expect(fake.docVersions, [3, 4]);
  });
}
