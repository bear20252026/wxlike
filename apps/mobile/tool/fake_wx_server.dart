// 移动端验收用的假服务端（独立进程）。
//
// 为什么必须独立进程：绑定层的调用在 Dart 侧是**同步**的（`lib/src/service.dart`
// 顶部写了这个取舍）。主 isolate 卡在 FFI 里的时候，同进程内的 `HttpServer`
// 根本没时间 accept —— 于是每个请求都"超时"，看起来像网络坏了，其实是自己
// 把自己锁住了。真机上没有这个问题（服务端在另一台机器），但测试里要么换进程，
// 要么把 FFI 挪进 isolate。
//
// 用法：
//   dart run tool/fake_wx_server.dart
// stdout 逐行输出 JSON：
//   {"port":12345}                  启动完成
//   {"event":"send","body":"…"}     收到一次 POST /api/conv/*/send
//   {"event":"device","body":"…"}   收到一次 POST /api/devices
// stdin 逐行输入命令：
//   refuse   之后的 /send 一律回 403（测"服务端明确拒绝"）
//   allow    恢复正常
import 'dart:convert';
import 'dart:io';

bool refuseSend = false;

/// stdout 接的是管道，Dart 不会按行刷出去；不显式 flush，父进程会一直等不到
/// `{"port":…}` 这一行。
void emit(Map<String, Object> event) {
  stdout
    ..writeln(jsonEncode(event))
    ..flush();
}

Future<void> main(List<String> args) async {
  final server = await HttpServer.bind(InternetAddress.loopbackIPv4, 0);
  emit({'port': server.port});
  stdin
      .transform(utf8.decoder)
      .transform(const LineSplitter())
      .listen((line) => refuseSend = line.trim() == 'refuse');
  server.listen((request) => _handle(request));
}

Future<void> _reply(HttpRequest request, int status, Object body) async {
  request.response
    ..statusCode = status
    ..headers.contentType = ContentType.json
    ..write(jsonEncode(body));
  await request.response.close();
}

Future<void> _handle(HttpRequest request) async {
  final path = request.uri.path;
  final body = request.method == 'POST' ? await utf8.decoder.bind(request).join() : '';
  // `/api/conv/{conv}/…` 与 `/api/history/{conv}` 才有的那一段，
  // 别在 /api/login 这种两段路径上取（取越界会让整个 handler 抛掉，
  // 连接被关，客户端只看到 "Unexpected EOF"）
  final segments = path.split('/');
  final conv = segments.length > 3 ? segments[3] : '';
  if (path == '/api/login' || path == '/api/register') {
    await _reply(request, 200, {'user_id': 'alice', 'token': 'tok-1'});
  } else if (path == '/api/me') {
    await _reply(request, 200, {'user_id': 'alice'});
  } else if (path == '/api/groups/mine') {
    await _reply(request, 200, [
      {'id': 'g-1', 'name': '项目组', 'member_count': 3},
    ]);
  } else if (path == '/api/friends') {
    await _reply(request, 200, [
      {'friend_id': 'bob'},
    ]);
  } else if (path == '/api/devices') {
    emit({"event": "device", "body": body});
    request.response.statusCode = HttpStatus.noContent;
    await request.response.close();
  } else if (path.endsWith('/unread') || path.endsWith('/read')) {
    final read = path.endsWith('/read');
    await _reply(request, 200, {
      'conv': conv,
      'owner': 'direct',
      'head_seq': 7,
      'read_seq': read ? 7 : 5,
      'unread': read ? 0 : 2,
    });
  } else if (path.endsWith('/send')) {
    if (refuseSend) {
      await _reply(request, 403, {'error': 'you are not a member'});
      return;
    }
    emit({"event": "send", "body": body});
    final key = (jsonDecode(body)['idempotency_key'] ?? '').toString();
    await _reply(request, 200, {
      'msg_id': 'srv-${key.hashCode.toRadixString(16)}',
      'conv_id': conv,
      'accepted': true,
    });
  } else if (path.startsWith('/api/history/')) {
    await _reply(request, 200, {
      'conv_id': conv,
      'messages': [
        {
          'conv_id': conv, 'seq': 5, 'sender': 'bob', 'msg_id': 'h5',
          'sent_at_ms': 1700000000005, 'withdrawn': false, 'kind': 'text',
          'text': '在',
        },
        {
          'conv_id': conv, 'seq': 6, 'sender': 'alice', 'msg_id': 'h6',
          'sent_at_ms': 1700000000006, 'withdrawn': false, 'kind': 'recall',
        },
      ],
      'next_before_seq': 4,
    });
  } else {
    await _reply(request, 404, {'error': 'no route $path'});
  }
}
