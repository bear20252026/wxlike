// 契约 fixture 的 Dart 端一致性检查。
//
//   dart run crates/types/fixtures/run_conformance.dart
//
// 规则与 Rust / JS 两端一致：解析 `value`，按"对象键升序、无空白、数字一律
// 字符串"输出规范化 JSON，与文件里的 `canonical` 逐字节比对。
//
// 为什么 Dart 也要单独跑：Dart 的 `int` 在 VM 上是 64 位（精确），在 web 上
// 是 double（不精确）。同一个 fixture 在两种编译目标下必须给出同样的结果，
// 所以金额/序列号在契约里一律是字符串——这个脚本就是那条规则的守门人。

import 'dart:convert';
import 'dart:io';

String canonical(Object? value) {
  if (value is List) {
    return '[${value.map(canonical).join(',')}]';
  }
  if (value is Map) {
    final keys = value.keys.map((dynamic k) => k.toString()).toList()..sort();
    return '{${keys.map((String k) => '${jsonEncode(k)}:${canonical(value[k])}').join(',')}}';
  }
  return jsonEncode(value);
}

void main() {
  final fixtures = Directory.fromUri(Platform.script.resolve('contract-v1/'));
  if (!fixtures.existsSync()) {
    stderr.writeln('找不到 fixture 目录：${fixtures.path}');
    exit(2);
  }

  final files = fixtures
      .listSync()
      .whereType<File>()
      .where((File f) => f.path.endsWith('.json'))
      .toList()
    ..sort((File a, File b) => a.path.compareTo(b.path));

  final failures = <String>[];
  var checked = 0;
  for (final file in files) {
    final doc = jsonDecode(file.readAsStringSync()) as Map<String, dynamic>;
    final got = canonical(doc['value']);
    if (got != doc['canonical']) {
      failures.add('${file.path}: 规范化不一致\n  期望 ${doc['canonical']}\n  实际 $got');
      continue;
    }
    checked += 1;
  }

  // 大数专项：Dart 的 BigInt 必须精确还原 2^53+1
  final money = jsonDecode(File('${fixtures.path}money.big.json').readAsStringSync())
      as Map<String, dynamic>;
  final minor = (money['value'] as Map<String, dynamic>)['minor'] as String;
  if (BigInt.parse(minor) != BigInt.from(9007199254740993)) {
    failures.add('money.big: BigInt 解析丢了精度（$minor）');
  }

  if (failures.isNotEmpty) {
    stderr.writeln(failures.join('\n'));
    exit(1);
  }
  stdout.writeln('ok: $checked 个 fixture 在 Dart 端规范化结果与 Rust 端一致');
}
