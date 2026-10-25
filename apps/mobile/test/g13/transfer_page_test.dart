import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:wxlike_mobile/features/commerce/transfer_page.dart';
import 'package:wxlike_mobile/features/content/g13/core.dart';
import 'package:wxlike_mobile/features/content/g13/models.dart';
import 'package:wxlike_mobile/features/content/g13/ports.dart';
import 'package:wxlike_mobile/features/content/g13/unavailable_ports.dart';

import 'g13_support.dart';

class _FakeCommerce extends UnavailableCommercePort {
  int prepareCalls = 0;
  int executeCalls = 0;
  TransferDraft? lastDraft;
  String? lastPrepareKey;
  String? lastExecuteKey;
  String executeState = 'SUCCEEDED';
  Completer<void>? gate;

  @override
  Future<List<BankCard>> bankCards() async => const [];

  @override
  Future<PaymentIntent> prepareTransfer(
    TransferDraft draft, {
    required String idempotencyKey,
  }) async {
    prepareCalls++;
    lastDraft = draft;
    lastPrepareKey = idempotencyKey;
    return PaymentIntent(
      id: 'intent-1',
      payee: draft.payeeName,
      amount: draft.amount,
      state: 'PENDING',
      expiresAt: DateTime.utc(2027, 1, 1),
      confirmation: ActionConfirmation(
        confirmToken: 'token-1',
        summary: '转账给 ${draft.payeeName}',
        expiresAt: DateTime.utc(2027, 1, 1),
      ),
    );
  }

  @override
  Future<PaymentIntent> executePayment(
    String intentId, {
    required String idempotencyKey,
  }) async {
    executeCalls++;
    lastExecuteKey = idempotencyKey;
    if (gate != null) await gate!.future;
    return PaymentIntent(
      id: intentId,
      payee: 'Bob',
      amount: const Money(minor: 10000, currency: 'CNY'),
      state: executeState,
      expiresAt: DateTime.utc(2027, 1, 1),
      confirmation: null,
    );
  }
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
              builder: (_) => TransferPage(
                ports: ports,
                convId: 'c-1',
                payeeId: 'u-2',
                payeeName: 'Bob',
              ),
            ),
          ),
          child: const Text('open'),
        ),
      ),
    );
  }
}

Future<void> _open(WidgetTester tester, _FakeCommerce fake) async {
  await tester.pumpWidget(
    MaterialApp(
      home: _Host(ports: testPorts(commerce: fake)),
    ),
  );
  await tester.tap(find.text('open'));
  await tester.pumpAndSettle();
}

Future<void> _confirmTransfer(WidgetTester tester) async {
  await tester.tap(find.widgetWithText(FilledButton, '转账'));
  await tester.pump();
  await tester.pump(const Duration(milliseconds: 400));
  await tester.tap(find.widgetWithText(FilledButton, '确认转账'));
  await tester.pump(const Duration(milliseconds: 400));
}

void main() {
  testWidgets('成功：金额按最小单位提交，SUCCEEDED 后才回会话', (tester) async {
    final fake = _FakeCommerce();
    await _open(tester, fake);

    await tester.tap(find.widgetWithText(ActionChip, '¥100'));
    await tester.pump();
    expect(find.text('CNY 100.00'), findsOneWidget);

    await _confirmTransfer(tester);
    await tester.pumpAndSettle();

    expect(fake.prepareCalls, 1);
    expect(fake.lastDraft!.amount.minor, 10000);
    expect(fake.lastDraft!.paymentMethodId, 'balance');
    expect(fake.lastPrepareKey, isNotEmpty);
    expect(fake.executeCalls, 1);
    expect(fake.lastExecuteKey, isNotEmpty);
    expect(find.text('open'), findsOneWidget);
  });

  testWidgets('取消确认：不调用 execute', (tester) async {
    final fake = _FakeCommerce();
    await _open(tester, fake);

    await tester.tap(find.widgetWithText(ActionChip, '¥200'));
    await tester.pump();
    await tester.tap(find.widgetWithText(FilledButton, '转账'));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 400));
    await tester.tap(find.text('取消'));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 400));

    expect(fake.prepareCalls, 1);
    expect(fake.executeCalls, 0);
    expect(find.text('转账'), findsWidgets);
  });

  testWidgets('非成功状态：不回会话，显示服务端状态', (tester) async {
    final fake = _FakeCommerce()..executeState = 'PENDING_RECONCILIATION';
    await _open(tester, fake);

    await tester.tap(find.widgetWithText(ActionChip, '¥100'));
    await tester.pump();
    await _confirmTransfer(tester);
    await tester.pumpAndSettle();

    expect(find.textContaining('支付状态：PENDING_RECONCILIATION'), findsOneWidget);
    expect(find.text('open'), findsNothing);
  });

  testWidgets('金额输入：拒绝第三位小数，全程整数解析', (tester) async {
    final fake = _FakeCommerce();
    await _open(tester, fake);

    for (final key in ['1', '.', '2', '3', '4']) {
      await tester.tap(find.text(key).last);
      await tester.pump();
    }

    expect(find.text('CNY 1.23'), findsOneWidget);
  });

  testWidgets('防重：确认后重复点击只执行一次支付', (tester) async {
    final gate = Completer<void>();
    final fake = _FakeCommerce()..gate = gate;
    await _open(tester, fake);

    await tester.tap(find.widgetWithText(ActionChip, '¥100'));
    await tester.pump();
    await _confirmTransfer(tester);
    await tester.pump(const Duration(milliseconds: 400));
    await tester.tap(find.byType(FilledButton).last);
    await tester.pump();

    expect(fake.prepareCalls, 1);
    expect(fake.executeCalls, 1);

    gate.complete();
    await tester.pumpAndSettle();
  });
}
