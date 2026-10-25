import 'package:flutter_test/flutter_test.dart';
import 'package:wxlike_mobile/features/content/g13/core.dart';
import 'package:wxlike_mobile/features/content/g13/models.dart';
import 'package:wxlike_mobile/features/content/g13/unavailable_ports.dart';

void main() {
  group('parseMinorAmount', () {
    test('整数与两位小数按最小单位解析', () {
      expect(parseMinorAmount('0'), 0);
      expect(parseMinorAmount('1'), 100);
      expect(parseMinorAmount('0.1'), 10);
      expect(parseMinorAmount('12.34'), 1234);
      expect(parseMinorAmount('.5'), 50);
    });

    test('超过两位小数、多小数点、空串返回 null', () {
      expect(parseMinorAmount(''), isNull);
      expect(parseMinorAmount('.'), isNull);
      expect(parseMinorAmount('1.234'), isNull);
      expect(parseMinorAmount('1.2.3'), isNull);
      expect(parseMinorAmount('-1'), isNull);
      expect(parseMinorAmount('abc'), isNull);
    });

    test('不做 double 舍入：0.1 + 0.2 的输入分别精确解析', () {
      expect(parseMinorAmount('0.1')! + parseMinorAmount('0.2')!, 30);
    });
  });

  group('幂等键', () {
    test('每次生成的键互不相同且非空', () {
      final a = newIdempotencyKey();
      final b = newIdempotencyKey();
      expect(a, isNotEmpty);
      expect(b, isNotEmpty);
      expect(a, isNot(b));
    });
  });

  group('未接线端口', () {
    test('抛 unavailable，而不是返回空结果或静态数据', () async {
      final ports = unavailablePorts();
      await expectLater(
        ports.content.moments(),
        throwsA(
          isA<G13Failure>().having(
            (e) => e.kind,
            'kind',
            G13FailureKind.unavailable,
          ),
        ),
      );
      await expectLater(
        ports.search.history(),
        throwsA(
          isA<G13Failure>().having(
            (e) => e.kind,
            'kind',
            G13FailureKind.unavailable,
          ),
        ),
      );
      await expectLater(
        ports.qr.myCode(QrStyle.spring, idempotencyKey: 'k'),
        throwsA(
          isA<G13Failure>().having((e) => e.code, 'code', 'UPSTREAM_NOT_WIRED'),
        ),
      );
    });

    test('流式端口以错误流结束，不伪装成空事件流', () async {
      final ports = unavailablePorts();
      await expectLater(
        ports.live.updates('discover'),
        emitsError(isA<G13Failure>()),
      );
      await expectLater(ports.calls.updates(), emitsError(isA<G13Failure>()));
    });
  });
}
