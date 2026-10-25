import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:wxlike_mobile/features/commerce/wallet_page.dart';
import 'package:wxlike_mobile/features/content/g13/core.dart';
import 'package:wxlike_mobile/features/content/g13/models.dart';
import 'package:wxlike_mobile/features/content/g13/unavailable_ports.dart';

import 'g13_support.dart';

class _FakeWallet extends UnavailableCommercePort {
  _FakeWallet(this.account);

  WalletAccount account;
  Object? error;
  int calls = 0;

  @override
  Future<WalletAccount> wallet() async {
    calls++;
    if (error != null) throw error!;
    return account;
  }
}

void main() {
  testWidgets('真钱与虚拟币分开展示，冻结状态可见', (tester) async {
    final fake = _FakeWallet(
      const WalletAccount(
        balance: Money(minor: 12345, currency: 'CNY'),
        virtualBalance: Money(minor: 500, currency: 'COIN'),
        currency: 'CNY',
        frozen: true,
      ),
    );
    await tester.pumpWidget(
      MaterialApp(
        home: WalletPage(ports: testPorts(commerce: fake)),
      ),
    );
    await tester.pumpAndSettle();

    expect(find.text('CNY 123.45'), findsOneWidget);
    expect(find.text('虚拟币 COIN'), findsOneWidget);
    expect(find.text('COIN 5.00'), findsOneWidget);
    expect(find.text('账户已冻结，资金操作暂不可用'), findsOneWidget);
    expect(find.text('66.66'), findsNothing);
  });

  testWidgets('刷新以服务端新值为准', (tester) async {
    final fake = _FakeWallet(
      const WalletAccount(
        balance: Money(minor: 100, currency: 'CNY'),
        virtualBalance: Money(minor: 0, currency: 'COIN'),
        currency: 'CNY',
        frozen: false,
      ),
    );
    await tester.pumpWidget(
      MaterialApp(
        home: WalletPage(ports: testPorts(commerce: fake)),
      ),
    );
    await tester.pumpAndSettle();
    expect(find.text('CNY 1.00'), findsOneWidget);

    fake.account = const WalletAccount(
      balance: Money(minor: 250, currency: 'CNY'),
      virtualBalance: Money(minor: 0, currency: 'COIN'),
      currency: 'CNY',
      frozen: false,
    );
    await tester.tap(find.byIcon(Icons.refresh));
    await tester.pumpAndSettle();

    expect(fake.calls, 2);
    expect(find.text('CNY 2.50'), findsOneWidget);
  });

  testWidgets('未授权：显示重新登录', (tester) async {
    final fake =
        _FakeWallet(
            const WalletAccount(
              balance: Money(minor: 0, currency: 'CNY'),
              virtualBalance: Money(minor: 0, currency: 'COIN'),
              currency: 'CNY',
              frozen: false,
            ),
          )
          ..error = const G13Failure(
            G13FailureKind.unauthorized,
            'UNAUTHORIZED',
            '未登录',
          );
    await tester.pumpWidget(
      MaterialApp(
        home: WalletPage(ports: testPorts(commerce: fake)),
      ),
    );
    await tester.pumpAndSettle();

    expect(find.text('登录已失效，请重新登录'), findsOneWidget);
  });
}
