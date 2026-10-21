import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:wxlike_mobile/src/service.dart';

/// B1/B3 的**跨语言**冒烟：Dart 绑定 → cdylib → Rust → HTTP → 假服务端。
///
/// `cargo test -p wxlike-ffi` 走的是 rlib（同一个进程、同一套 Rust 类型），
/// 它验不到"绑定本身对不对"——字段名、错误抛出、i64/字符串的跨界都能在这里翻车。
/// 这个测试用宿主动态库（Windows 的 .dll / Linux 的 .so）真跑一遍 FFI。
///
/// 跑法（库路径从环境变量给，或用 worktree 里默认的 `target/debug`）：
///
/// ```sh
/// WXLIKE_FFI_LIB=target/debug/wxlike_ffi.dll flutter test test/ffi_smoke_test.dart
/// ```
///
/// 找不到库时**跳过**而不是失败——CI 上没有宿主动态库是常态，
/// 但这里不允许把"没跑到"记成"跑过了"。
/// `flutter test` 里 `Platform.resolvedExecutable` 是 `flutter_tester.exe`，
/// 不是 dart CLI —— 用它 spawn 子进程只会得到 "Dart kernel file not specified"。
/// dart SDK 从 `FLUTTER_ROOT` 推（flutter 工具跑测试时一定会设），或用
/// `WXLIKE_DART` 显式指。
String? _dartExecutable() {
  final configured = Platform.environment['WXLIKE_DART'];
  if (configured != null && configured.isNotEmpty && File(configured).existsSync()) {
    return configured;
  }
  final root = Platform.environment['FLUTTER_ROOT'];
  if (root == null || root.isEmpty) return null;
  final name = Platform.isWindows ? 'dart.exe' : 'dart';
  final file = File('$root${Platform.pathSeparator}bin${Platform.pathSeparator}cache'
      '${Platform.pathSeparator}dart-sdk${Platform.pathSeparator}bin${Platform.pathSeparator}$name');
  return file.existsSync() ? file.path : null;
}

/// 假服务端跑在**子进程**里。
///
/// 绑定层在 Dart 侧是同步调用，主 isolate 卡在 FFI 里时，同进程的
/// `HttpServer` 没时间 accept —— 每个请求都会"超时"，看着像网络坏了，
/// 其实是自己锁住自己。脚本是 `tool/fake_wx_server.dart`（真机验收也能用它
/// 当桩服务端）。
class FakeWx {
  Process? _process;
  final List<String> sendBodies = [];
  final List<String> deviceBodies = [];
  String _stdout = '';

  Future<String> start() async {
    final dart = _dartExecutable();
    if (dart == null) {
      throw StateError('找不到 dart CLI（设 FLUTTER_ROOT 或 WXLIKE_DART）');
    }
    final process = await Process.start(dart, ['run', 'tool/fake_wx_server.dart']);
    _process = process;
    final port = Completer<int>();
    process.stdout.transform(utf8.decoder).listen((chunk) {
      _stdout += chunk;
      for (final line in _stdout.split('\n')) {
        if (!line.trim().startsWith('{')) continue;
        final map = jsonDecode(line) as Map<String, dynamic>;
        if (map.containsKey('port')) {
          if (!port.isCompleted) port.complete(map['port'] as int);
        } else if (map['event'] == 'send') {
          sendBodies.add(map['body'] as String);
        } else if (map['event'] == 'device') {
          deviceBodies.add(map['body'] as String);
        }
      }
      _stdout = '';
    });
    process.stderr.transform(utf8.decoder).listen(stderr.write);
    final bound = await port.future.timeout(const Duration(seconds: 60));
    return 'http://127.0.0.1:$bound';
  }

  Future<void> _command(String line) async {
    _process!.stdin.writeln(line);
    await _process!.stdin.flush();
  }

  /// 事件是子进程打印的，父进程读到会有几毫秒滞后——断言计数前先等到位。
  /// （绑定层是同步调用，主 isolate 卡在 FFI 里的时候根本读不到管道。）
  Future<void> _waitFor(List<String> pool, int count) async {
    for (var i = 0; i < 250 && pool.length < count; i++) {
      await Future<void>.delayed(const Duration(milliseconds: 20));
    }
    expect(pool.length, count);
  }

  Future<void> waitForSendCount(int count) => _waitFor(sendBodies, count);

  Future<void> waitForDeviceCount(int count) => _waitFor(deviceBodies, count);

  Future<void> refuseSend() => _command('refuse');

  Future<void> allowSend() => _command('allow');

  Future<void> stop() async {
    _process?.kill();
    _process = null;
  }
}

String? _hostLibrary() {
  final configured = Platform.environment['WXLIKE_FFI_LIB'];
  if (configured != null && configured.isNotEmpty) {
    return File(configured).existsSync() ? configured : null;
  }
  const names = {
    'windows': 'wxlike_ffi.dll',
    'linux': 'libwxlike_ffi.so',
    'macos': 'libwxlike_ffi.dylib',
  };
  final name = names[Platform.operatingSystem];
  if (name == null) return null;
  // 当前工作目录是 apps/mobile：../.. 是 worktree 自带的 target，
  // 再往上是共享 CARGO_TARGET_DIR 时的主 checkout
  for (final root in ['../../target/debug', '../../../../wxlike/target/debug']) {
    final file = File('$root/$name');
    if (file.existsSync()) return file.absolute.path;
  }
  return null;
}

/// 一个"必然连不上"的地址：绑上立刻关，回环上连它会得到 connection refused。
Future<String> deadBase() async {
  final socket = await ServerSocket.bind(InternetAddress.loopbackIPv4, 0);
  final port = socket.port;
  await socket.close();
  return 'http://127.0.0.1:$port';
}

File tempDb(String tag) {
  final file = File('${Directory.systemTemp.path}/wxlike-ffi-smoke-$tag-$pid.sqlite');
  if (file.existsSync()) file.deleteSync();
  return file;
}

void main() {
  final library = _hostLibrary();
  final reason = library == null ? '找不到宿主动态库，设 WXLIKE_FFI_LIB 指向它' : false;

  test('FFI 跨界冒烟：登录 → 发送 → 断网读缓存 → 恢复后补发同一条', () async {
    final fake = FakeWx();
    final base = await fake.start();
    addTearDown(fake.stop);
    final db = tempDb('main');

    final online = FfiWxlikeService.at(library!, base, db.path);
    addTearDown(online.close);

    final session = await online.login('alice', 'pw');
    expect(session.signedIn, isTrue);
    expect(session.userId, 'alice');

    final live = await online.connect();
    expect(live.state, LiveState.online);

    final list = await online.conversations();
    expect(list.fromCache, isFalse);
    expect(list.conversations.map((r) => r.conv), containsAll(<String>['g-1', 'd-alice~bob']));
    expect(list.conversations.firstWhere((r) => r.conv == 'g-1').unread, 2);

    final page = await online.history('d-alice~bob', 0, 50);
    expect(page.fromCache, isFalse);
    expect(page.messages.length, 2);
    // 撤回文案由 Rust 按"谁撤的"决定，界面不参与判断
    expect(page.messages.firstWhere((m) => m.seq == 6).text, '你撤回了一条消息');
    expect(page.messages.firstWhere((m) => m.seq == 6).mine, isTrue);

    final sent = await online.sendText('d-alice~bob', '在线发的');
    expect(sent.state, OutboxState.sent);
    expect(sent.msgId, isNotEmpty);
    await fake.waitForSendCount(1);

    // 推送 token 注册走的是 POST /api/devices
    await online.registerPushToken('android', 'fcm-abc');
    await fake.waitForDeviceCount(1);
    expect(fake.deviceBodies.single, contains('fcm-abc'));

    // ---- 断网：换一个"指向死地址、共用同一个本地库"的客户端 = 重启后没信号 ----
    final dead = await deadBase();
    final offline = FfiWxlikeService.at(library, dead, db.path);
    addTearDown(offline.close);
    expect(offline.session().signedIn, isTrue, reason: 'token 应该已经落库');

    final cached = await offline.history('d-alice~bob', 0, 50);
    expect(cached.fromCache, isTrue, reason: '断网时该回落到本地缓存');
    expect(
      cached.messages.where((m) => m.text == '在线发的').length,
      1,
      reason: '发送成功的那条在本地只有一行，不会重复',
    );
    final cachedList = await offline.conversations();
    expect(cachedList.fromCache, isTrue);
    expect(cachedList.conversations, isNotEmpty);
    expect((await offline.liveInfo()).state, LiveState.offline);

    final queued = await offline.sendText('d-alice~bob', '地铁里发的');
    expect(queued.state, OutboxState.pending);
    final status = await offline.outboxStatus();
    expect(status.pending, 1);
    expect(status.entries.single.text, '地铁里发的');

    // ---- 恢复：另一个客户端（同一个库、活的地址）补发。幂等键必须还是那个 task_id ----
    final recovered = FfiWxlikeService.at(library, base, db.path);
    addTearDown(recovered.close);
    // 队列的退避窗口：`record_failure` 把 next_retry_at 推后了 1 秒，
    // 没到点就不该发——提前撞墙等于白烧一次尝试（一共只有 5 次）。
    await Future<void>.delayed(const Duration(milliseconds: 1200));
    final report = await recovered.flushOutbox();
    expect(report.sent, 1);
    await fake.waitForSendCount(2);
    expect(
      fake.sendBodies.last,
      contains(queued.taskId),
      reason: '补发必须带同一个幂等键，否则重试就成两条消息',
    );
    expect((await recovered.outboxStatus()).pending, 0);

    // ---- 服务端明确拒绝：不重试，直接判死 ----
    await fake.refuseSend();
    final denied = await recovered.sendText('g-9', '不该发');
    expect(denied.state, OutboxState.failed);
    expect(denied.error, contains('not a member'));
    expect((await recovered.outboxStatus()).failed, 1);
    await fake.allowSend();

    // ---- 登出：凭据、本地消息、队列一起清 ----
    await recovered.logout();
    expect(recovered.session().signedIn, isFalse);
    expect((await recovered.outboxStatus()).pending, 0);
    final after = FfiWxlikeService.at(library, dead, db.path);
    addTearDown(after.close);
    expect((await after.conversations()).conversations, isEmpty, reason: '会话缓存要跟着清');
    expect((await after.history('d-alice~bob', 0, 50)).messages, isEmpty,
        reason: '本地正文要跟着凭据一起走');
  }, skip: reason);

  test('离线时错误分类是网络类而不是"未知"（FFI 层看到的类型要对）', () async {
    final dead = await deadBase();
    final service = FfiWxlikeService.at(library!, dead, tempDb('net').path);
    addTearDown(service.close);
    await expectLater(
      () async => service.login('alice', 'pw'),
      throwsA(isA<WxlikeErrorExceptionNetwork>()),
    );
  }, skip: reason);
}
