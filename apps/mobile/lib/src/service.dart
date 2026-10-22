/// 移动端与 Rust 的唯一接点。
///
/// 分层只有一条规则：**这一层以上都是界面，这一层以下全是 Rust**。
/// `WxlikeService` 是界面能问的全部问题；`FfiWxlikeService` 把它原样转给
/// UniFFI 生成的绑定，一个判断都不加。未读怎么算、消息怎么排序、离线时给什么、
/// 发不出去进不进队列——全在 `crates/ffi` 里（见那里的模块注释）。
///
/// **所有阻塞调用都跑在后台 isolate 上**（issue #17）。
///
/// 绑定层是同步的：`crates/client` 用 ureq，单次超时上限 10 秒。以前这些调用
/// 直接跑在 UI isolate 上，服务端慢几秒整页就卡住——滚动、取消、进度全都停。
/// 现在只有一个长期存活的 worker isolate 持有 `WxlikeClient`，界面侧通过
/// `SendPort` 发命令、等 `Completer`。
///
/// 为什么不是"每次调用起一个 isolate"：`WxlikeClient` 是 uniffi 的 object，
/// 背后是 Rust 的 Arc 和一条 SQLite 连接，**跨 isolate 传不了**，而且每次新建
/// 都要重开库、重起补发线程。所以 worker 必须长期存活。
///
/// 能跨界的是返回值：`SessionInfo` / `MessagePage` 这些都是 uniffi 的 record
/// （纯 Dart 类），枚举与异常类也一样。`session()` 因此做成**本地缓存**并保持
/// 同步——它只读本地状态，没有阻塞风险。
library;

import 'dart:async';
import 'dart:io' show Platform;
import 'dart:isolate';

import 'package:path_provider/path_provider.dart';

import 'rust/wxlike_ffi.dart';
// 同一个库再带一次前缀：顶层函数与类的构造入口用 `rust.` 写法，读起来清楚。
import 'rust/wxlike_ffi.dart' as rust;

export 'rust/wxlike_ffi.dart'
    show
        ChatMessage,
        ConversationList,
        ConversationRow,
        FlushReport,
        LiveInfo,
        LiveState,
        MessagePage,
        OutboxEntry,
        OutboxState,
        OutboxSummary,
        SendOutcome,
        SessionInfo,
        UnreadState,
        WxlikeErrorException,
        WxlikeErrorExceptionNetwork,
        WxlikeErrorExceptionPermission,
        WxlikeErrorExceptionUnauthorized,
        WxlikeErrorExceptionUnexpected;

/// 一屏加载的最长历史条数。
const int pageSize = 50;

/// 直聊会话 id 的算法在 Rust（它是协议的一部分），这里只是转一下。
String directConvId(String a, String b) => rust.directConvId(a, b);

abstract class WxlikeService {
  SessionInfo session();

  Future<SessionInfo> register(String name, String password);

  Future<SessionInfo> login(String name, String password);

  Future<void> logout();

  /// 验证凭据 + 开始补发离线期间攒下的消息。
  Future<LiveInfo> connect();

  Future<void> disconnect();

  Future<LiveInfo> liveInfo();

  Future<ConversationList> conversations();

  /// `beforeSeq = 0` 表示"最新一页"（与服务端 `conv.history` 同语义）。
  Future<MessagePage> history(String conv, int beforeSeq, int limit);

  Future<UnreadState> unread(String conv);

  Future<UnreadState> markRead(String conv, int uptoSeq);

  Future<SendOutcome> sendText(String conv, String text);

  Future<OutboxSummary> outboxStatus();

  Future<FlushReport> flushOutbox();

  Future<OutboxSummary> retryOutbox(String taskId);

  Future<bool> dropOutbox(String taskId);

  Future<void> registerPushToken(String platform, String token);

  void close();
}

/// 原生库的文件名：Android 上 jniLibs 里的 `libwxlike_ffi.so`，
/// 桌面/测试机上是对应的 dll/dylib/so。
String _libraryFileName() {
  if (Platform.isAndroid) return 'libwxlike_ffi.so';
  if (Platform.isWindows) return 'wxlike_ffi.dll';
  return 'libwxlike_ffi.so';
}

/// 后台 worker：持有唯一的 `WxlikeClient`，所有阻塞调用在这里跑。
///
/// 协议是 `[id, op, args]` → `[id, ok, value]`。`id` 保证并发调用能各自对上
/// 自己的 `Completer`（界面经常同时拉会话列表和未读数）。
class _FfiWorker {
  _FfiWorker._(this._isolate, this._replies, this._commands);

  final Isolate _isolate;
  final ReceivePort _replies;
  final SendPort _commands;
  final Map<int, Completer<Object?>> _pending = {};
  int _next = 1;
  bool _closed = false;

  static Future<_FfiWorker> start({
    String? libraryPath,
    required String base,
    required String dbPath,
  }) async {
    final replies = ReceivePort();
    final failures = ReceivePort();
    final ready = Completer<SendPort>();

    final isolate = await Isolate.spawn(
      _workerMain,
      [replies.sendPort, libraryPath, base, dbPath],
      onError: failures.sendPort,
      debugName: 'wxlike-ffi',
    );

    late final _FfiWorker worker;
    replies.listen((message) {
      if (message is List && message.isNotEmpty) {
        if (message.first == 'ready') {
          if (!ready.isCompleted) ready.complete(message[1] as SendPort);
          return;
        }
        if (message.first == 'init-error') {
          if (!ready.isCompleted) {
            ready.completeError(StateError('初始化 FFI worker 失败：${message[1]}'));
          }
          return;
        }
      }
      if (message is List && message.length == 3 && message[0] is int) {
        final completer = worker._pending.remove(message[0] as int);
        if (completer == null || completer.isCompleted) return;
        if (message[1] == true) {
          completer.complete(message[2]);
        } else {
          completer.completeError(message[2] as Object);
        }
      }
    });
    failures.listen((message) {
      final text = message is List && message.isNotEmpty ? '${message.first}' : '$message';
      if (!ready.isCompleted) {
        ready.completeError(StateError('FFI worker 崩溃：$text'));
      }
    });

    final commands = await ready.future;
    worker = _FfiWorker._(isolate, replies, commands);
    return worker;
  }

  Future<Object?> call(String op, [List<Object?> args = const []]) {
    if (_closed) {
      return Future.error(StateError('service 已关闭'));
    }
    final id = _next++;
    final completer = Completer<Object?>();
    _pending[id] = completer;
    _commands.send([id, op, args]);
    return completer.future;
  }

  /// 关掉 worker：先让等在途的调用各自失败，再硬杀 isolate。
  ///
  /// 硬杀是必要的：卡在 ureq 里的调用不会因为"收到退出消息"就松手，
  /// 而 isolate 一杀，它占的线程与连接也就跟着没了。
  void shutdown() {
    if (_closed) return;
    _closed = true;
    for (final completer in _pending.values) {
      if (!completer.isCompleted) {
        completer.completeError(StateError('service 已关闭'));
      }
    }
    _pending.clear();
    _isolate.kill(priority: Isolate.immediate);
    _replies.close();
  }
}

/// worker isolate 的入口。**必须是顶层函数**（`Isolate.spawn` 的要求）。
void _workerMain(List<Object?> args) {
  final toMain = args[0] as SendPort;
  final libraryPath = args[1] as String?;
  final base = args[2] as String;
  final dbPath = args[3] as String;
  final inbox = ReceivePort();

  late final rust.WxlikeClient client;
  try {
    if (libraryPath != null) {
      rust.configureDefaultBindings(libraryPath: libraryPath);
    }
    client = rust.WxlikeClient.create(base, dbPath);
  } catch (error) {
    toMain.send(['init-error', '$error']);
    inbox.close();
    return;
  }
  toMain.send(['ready', inbox.sendPort]);

  inbox.listen((message) {
    final list = message as List<Object?>;
    final id = list[0] as int;
    final op = list[1] as String;
    final params = list[2] as List<Object?>;
    try {
      toMain.send([id, true, _dispatch(client, op, params)]);
    } catch (error) {
      // 异常对象是 uniffi 生成的普通类（只带字符串字段），可以跨界；
      // 万一哪天变成不可发送的，至少不要把 worker 打死。
      try {
        toMain.send([id, false, error]);
      } catch (_) {
        toMain.send([id, false, StateError('$error')]);
      }
    }
  });
}

/// 命令表。新增 FFI 方法时只改这里和 `WxlikeService` 的转发。
Object? _dispatch(rust.WxlikeClient client, String op, List<Object?> a) {
  switch (op) {
    case 'session':
      return client.session();
    case 'register':
      return client.register(a[0] as String, a[1] as String);
    case 'login':
      return client.login(a[0] as String, a[1] as String);
    case 'logout':
      client.logout();
      return null;
    case 'connect':
      return client.connect();
    case 'disconnect':
      client.disconnect();
      return null;
    case 'liveInfo':
      return client.liveInfo();
    case 'conversations':
      return client.conversations();
    case 'history':
      return client.history(a[0] as String, a[1] as int, a[2] as int);
    case 'unread':
      return client.unread(a[0] as String);
    case 'markRead':
      return client.markRead(a[0] as String, a[1] as int);
    case 'sendText':
      return client.sendText(a[0] as String, a[1] as String);
    case 'outboxStatus':
      return client.outboxStatus();
    case 'flushOutbox':
      return client.flushOutbox();
    case 'retryOutbox':
      return client.retryOutbox(a[0] as String);
    case 'dropOutbox':
      return client.dropOutbox(a[0] as String);
    case 'registerPushToken':
      client.registerPushToken(a[0] as String, a[1] as String);
      return null;
    case 'close':
      client.close();
      return null;
    default:
      throw StateError('unknown FFI op: $op');
  }
}

class FfiWxlikeService implements WxlikeService {
  FfiWxlikeService._(this._worker, this._session);

  final _FfiWorker _worker;

  /// 本地缓存：`session()` 保持同步（它只读本地状态，没有阻塞风险）。
  /// 每次登录/登出/注册都刷新它。
  SessionInfo _session;

  /// 打开客户端。`serverBase` 形如 `http://127.0.0.1:8099`。
  ///
  /// 本地库放在应用私有目录：消息正文与 token 在本地是明文（与桌面端同口径），
  /// 所以这个目录必须是非共享的。
  static Future<FfiWxlikeService> open(String serverBase) async {
    final dir = await getApplicationDocumentsDirectory();
    final db = '${dir.path}${Platform.pathSeparator}wxlike.sqlite';
    return _open(_libraryFileName(), serverBase, db);
  }

  /// 给测试与 host 侧冒烟用：直接指定库路径与数据库路径，绕开 path_provider。
  ///
  /// 是 `Future` 而不是同步构造：worker 要起 isolate 并建好客户端才算打开。
  static Future<FfiWxlikeService> at(
    String libraryPath,
    String serverBase,
    String dbPath,
  ) =>
      _open(libraryPath, serverBase, dbPath);

  static Future<FfiWxlikeService> _open(
    String libraryPath,
    String serverBase,
    String dbPath,
  ) async {
    final worker = await _FfiWorker.start(
      libraryPath: libraryPath,
      base: serverBase,
      dbPath: dbPath,
    );
    final session = await worker.call('session') as SessionInfo;
    return FfiWxlikeService._(worker, session);
  }

  Future<T> _call<T>(String op, [List<Object?> args = const []]) async =>
      await _worker.call(op, args) as T;

  @override
  SessionInfo session() => _session;

  @override
  Future<SessionInfo> register(String name, String password) async =>
      _session = await _call<SessionInfo>('register', [name, password]);

  @override
  Future<SessionInfo> login(String name, String password) async =>
      _session = await _call<SessionInfo>('login', [name, password]);

  @override
  Future<void> logout() async {
    await _call<void>('logout');
    // 登出之后以 Rust 的答案为准（token 清没清、user id 还留不留）
    _session = await _call<SessionInfo>('session');
  }

  @override
  Future<LiveInfo> connect() async => _call<LiveInfo>('connect');

  @override
  Future<void> disconnect() async => _call<void>('disconnect');

  @override
  Future<LiveInfo> liveInfo() async => _call<LiveInfo>('liveInfo');

  @override
  Future<ConversationList> conversations() async => _call<ConversationList>('conversations');

  @override
  Future<MessagePage> history(String conv, int beforeSeq, int limit) async =>
      _call<MessagePage>('history', [conv, beforeSeq, limit]);

  @override
  Future<UnreadState> unread(String conv) async => _call<UnreadState>('unread', [conv]);

  @override
  Future<UnreadState> markRead(String conv, int uptoSeq) async =>
      _call<UnreadState>('markRead', [conv, uptoSeq]);

  @override
  Future<SendOutcome> sendText(String conv, String text) async =>
      _call<SendOutcome>('sendText', [conv, text]);

  @override
  Future<OutboxSummary> outboxStatus() async => _call<OutboxSummary>('outboxStatus');

  @override
  Future<FlushReport> flushOutbox() async => _call<FlushReport>('flushOutbox');

  @override
  Future<OutboxSummary> retryOutbox(String taskId) async =>
      _call<OutboxSummary>('retryOutbox', [taskId]);

  @override
  Future<bool> dropOutbox(String taskId) async => _call<bool>('dropOutbox', [taskId]);

  @override
  Future<void> registerPushToken(String platform, String token) async =>
      _call<void>('registerPushToken', [platform, token]);

  @override
  void close() => _worker.shutdown();
}

/// 错误 → 一句话。类别是 Rust 判的（`crates/ffi/src/error.rs`），
/// 这里只决定"给用户看哪一种说法"。
///
/// 类型叫 `WxlikeErrorException*`：UniFFI 把 Rust 的错误枚举生成两套——
/// 数据用 `WxlikeError*`（sealed class），跨界抛出来的是 `WxlikeErrorException*`
/// （`implements Exception`）。界面拿到的是后者；照着前者写 `catch` 会一条都抓不到。
String describeError(Object error) {
  if (error is WxlikeErrorExceptionNetwork) {
    return '连不上服务器：${error.detail}';
  }
  if (error is WxlikeErrorExceptionUnauthorized) {
    return '需要重新登录：${error.detail}';
  }
  if (error is WxlikeErrorExceptionPermission) {
    return '没有权限：${error.detail}';
  }
  if (error is WxlikeErrorExceptionUnexpected) {
    return '出错了：${error.detail}';
  }
  return '$error';
}
