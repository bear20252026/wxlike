import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:wxlike_mobile/features/content/g13/core.dart';
import 'package:wxlike_mobile/features/content/g13/models.dart';
import 'package:wxlike_mobile/features/content/g13/ports.dart';
import 'package:wxlike_mobile/features/search/search_page.dart';

import 'g13_support.dart';

class _FakeSearch implements SearchPort {
  _FakeSearch({this.result, this.error, this.historyItems = const []});

  PageResult<SearchHit>? result;
  Object? error;
  List<String> historyItems;
  Completer<void>? gate;
  int searchCalls = 0;
  int clearCalls = 0;
  String? lastQuery;
  String? lastType;
  String? lastClearKey;

  @override
  Future<PageResult<SearchHit>> search(
    String query, {
    String type = '',
    String cursor = '',
    int limit = 20,
  }) async {
    searchCalls++;
    lastQuery = query;
    lastType = type;
    if (gate != null) await gate!.future;
    if (error != null) throw error!;
    final page = result;
    if (page == null) return PageResult.empty<SearchHit>();
    if (cursor.isEmpty) return page;
    return PageResult<SearchHit>(
      items: const [],
      hasMore: false,
      snapshotVersion: page.snapshotVersion,
    );
  }

  @override
  Future<List<String>> history() async => historyItems;

  @override
  Future<void> clearHistory({required String idempotencyKey}) async {
    clearCalls++;
    lastClearKey = idempotencyKey;
    historyItems = const [];
  }
}

SearchHit _hit(String id, String title) => SearchHit(
  type: 'contact',
  id: id,
  title: title,
  snippet: '你好',
  source: '好友',
  available: true,
);

Future<void> _open(WidgetTester tester, _FakeSearch fake) async {
  await tester.pumpWidget(
    MaterialApp(
      home: SearchPage(ports: testPorts(search: fake)),
    ),
  );
  await tester.pump();
}

Future<void> _submit(WidgetTester tester, String text) async {
  await tester.enterText(find.byType(TextField), text);
  await tester.testTextInput.receiveAction(TextInputAction.search);
  await tester.pump();
}

void main() {
  testWidgets('成功：结果来自端口，历史可点击回填', (tester) async {
    final fake = _FakeSearch(
      result: PageResult<SearchHit>(
        items: [_hit('c-1', 'Alice')],
        hasMore: false,
        snapshotVersion: 3,
      ),
      historyItems: ['alice'],
    );
    await _open(tester, fake);
    expect(find.text('搜索历史'), findsOneWidget);

    await _submit(tester, 'alice');
    await tester.pumpAndSettle();

    expect(fake.searchCalls, 1);
    expect(fake.lastQuery, 'alice');
    expect(find.text('Alice'), findsOneWidget);
    expect(find.text('你好'), findsOneWidget);
  });

  testWidgets('空结果：明确显示没有找到，而不是空白', (tester) async {
    final fake = _FakeSearch(
      result: const PageResult<SearchHit>(
        items: [],
        hasMore: false,
        snapshotVersion: 1,
      ),
    );
    await _open(tester, fake);
    await _submit(tester, '不存在');
    await tester.pumpAndSettle();

    expect(find.textContaining('没有找到'), findsOneWidget);
  });

  testWidgets('参数错误：空查询不发起请求', (tester) async {
    final fake = _FakeSearch();
    await _open(tester, fake);
    await _submit(tester, '');
    await tester.pumpAndSettle();

    expect(find.text('请输入搜索内容'), findsOneWidget);
    expect(fake.searchCalls, 0);
  });

  testWidgets('未授权：显示重新登录且不给重试', (tester) async {
    final fake = _FakeSearch(
      error: const G13Failure(
        G13FailureKind.unauthorized,
        'UNAUTHORIZED',
        '未登录',
      ),
    );
    await _open(tester, fake);
    await _submit(tester, 'alice');
    await tester.pumpAndSettle();

    expect(find.text('登录已失效，请重新登录'), findsOneWidget);
    expect(find.text('重试'), findsNothing);
  });

  testWidgets('断网：可重试且复用同一个查询', (tester) async {
    final fake = _FakeSearch(
      error: const G13Failure(
        G13FailureKind.offline,
        'OFFLINE',
        '断网',
        retryable: true,
      ),
    );
    await _open(tester, fake);
    await _submit(tester, 'alice');
    await tester.pumpAndSettle();

    expect(find.text('网络已断开，请检查连接'), findsOneWidget);
    await tester.tap(find.text('重试'));
    await tester.pumpAndSettle();
    expect(fake.searchCalls, 2);
    expect(fake.lastQuery, 'alice');
  });

  testWidgets('防重：请求未返回时重复提交只发一次', (tester) async {
    final gate = Completer<void>();
    final fake = _FakeSearch(
      result: const PageResult<SearchHit>(
        items: [],
        hasMore: false,
        snapshotVersion: 1,
      ),
    )..gate = gate;
    await _open(tester, fake);

    await _submit(tester, 'alice');
    await tester.testTextInput.receiveAction(TextInputAction.search);
    await tester.pump();
    expect(fake.searchCalls, 1);

    gate.complete();
    await tester.pumpAndSettle();
  });

  testWidgets('清空历史：取消不发请求，确认只发一次并带幂等键', (tester) async {
    final fake = _FakeSearch(historyItems: ['alice']);
    await _open(tester, fake);

    await tester.tap(find.text('清空'));
    await tester.pumpAndSettle();
    await tester.tap(find.text('取消'));
    await tester.pumpAndSettle();
    expect(fake.clearCalls, 0);

    await tester.tap(find.text('清空'));
    await tester.pumpAndSettle();
    await tester.tap(find.widgetWithText(FilledButton, '清空'));
    await tester.pumpAndSettle();

    expect(fake.clearCalls, 1);
    expect(fake.lastClearKey, isNotEmpty);
    expect(find.text('还没有搜索记录'), findsOneWidget);
  });
}
