import 'package:flutter_test/flutter_test.dart';
import 'package:wxlike_mobile/features/auth/qr_approval/qr_payload.dart';
import 'package:wxlike_mobile/shell/contracts.dart';

import 'qr_support.dart';

void main() {
  group('登录码载荷解析', () {
    test('合法载荷拆出票号与 secret，可展示形态里只有占位符', () {
      final payload = QrScanPayload.parse(qrPayload);
      expect(payload.ticketId, qrTicketId);
      // secret 必须原样保留——它是 /scan 请求体里唯一能对上票的东西。
      expect(payload.secret, qrSecret);
      expect(payload.redacted, 'wxlike-qr://$qrTicketId/<redacted>');
      expect(payload.toString(), isNot(contains(qrSecret)));
    });

    test('首尾空白是扫码器的正常噪声，不算畸形', () {
      final payload = QrScanPayload.parse('  $qrPayload\n');
      expect(payload.ticketId, qrTicketId);
    });

    test('畸形载荷逐条给出可见原因', () {
      final cases = <String, String>{
        '': '为空',
        'https://example.com/login': 'scheme 不对',
        'wxlike://qr/$qrTicketId': '本应用深链但没有 secret',
        'wxlike-qr://$qrTicketId': '缺 secret',
        'wxlike-qr://$qrTicketId/': 'secret 段是空的',
        'wxlike-qr:///$qrSecret': '票号段是空的',
        'wxlike-qr://$qrTicketId/$qrSecret/extra': '路径段过多',
        'wxlike-qr://$qrTicketId/$qrSecret?next=1': '带 query',
        'wxlike-qr://$qrTicketId/$qrSecret#frag': '带 fragment',
        'wxlike-qr://$qrTicketId / $qrSecret': '中间有空白',
        'wxlike-qr://not-a-ticket/$qrSecret': '票号字符集不对',
        'wxlike-qr://$qrTicketId/zzzzzzzzzzzzzzzz': 'secret 不是 hex',
        'wxlike-qr://$qrTicketId/short': 'secret 太短',
      };
      for (final entry in cases.entries) {
        var thrown = false;
        try {
          QrScanPayload.parse(entry.key);
        } on G12Failure catch (failure) {
          thrown = true;
          // 必须有 code + 一句能给用户看的话：只抛"格式错误"四个字等于没修。
          expect(failure.code, 'QR_PAYLOAD_INVALID', reason: entry.value);
          expect(failure.kind, G12FailureKind.invalidArgument, reason: entry.value);
          expect(failure.message, isNotEmpty, reason: entry.value);
          expect(failure.displayText, isNotEmpty, reason: entry.value);
          expect(failure.displayText, isNot(contains(qrSecret)), reason: entry.value);
        }
        expect(thrown, isTrue, reason: '${entry.value}（${entry.key}）没有被判为畸形');
      }
    });

    test('tryParse 只给"能/不能"，不吞掉形状问题', () {
      expect(QrScanPayload.tryParse(qrPayload)?.ticketId, qrTicketId);
      expect(QrScanPayload.tryParse('wxlike-qr://$qrTicketId'), isNull);
    });

    test('secret 不出现在任何字符串表示与错误文案里', () {
      final payload = QrScanPayload.parse(qrPayload);
      final representations = <String>[
        payload.toString(),
        payload.redacted,
        payload.ticketId,
        // 票号里含在 payload 内，逐字符比对才有意义。
        '$payload',
      ];
      for (final text in representations) {
        expect(text.contains(qrSecret), isFalse, reason: text);
      }
      expect(QrScanPayload.secretPlaceholder, '<redacted>');
    });
  });
}
