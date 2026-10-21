/// 移动端与 Rust 的唯一接点。
///
/// 分层只有一条规则：**这一层以上都是界面，这一层以下全是 Rust**。
/// `WxlikeService` 是界面能问的全部问题；`FfiWxlikeService` 把它原样转给
/// UniFFI 生成的绑定，一个判断都不加。未读怎么算、消息怎么排序、离线时给什么、
/// 发不出去进不进队列——全在 `crates/ffi` 里（见那里的模块注释）。
///
/// 为什么方法是 `Future` 而实现是同步的：绑定层的调用是阻塞的（`crates/client`
/// 用 ureq，单次超时上限 10 秒），现在跑在 UI isolate 上。接口先按异步写好，
/// 将来把这个实现换成 isolate actor 时，界面代码一行都不用改。
library;

import 'dart:io' show Platform;

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

class FfiWxlikeService implements WxlikeService {
  FfiWxlikeService._(this._client);

  final rust.WxlikeClient _client;

  /// 打开客户端。`serverBase` 形如 `http://127.0.0.1:8099`。
  ///
  /// 本地库放在应用私有目录：消息正文与 token 在本地是明文（与桌面端同口径），
  /// 所以这个目录必须是非共享的。
  static Future<FfiWxlikeService> open(String serverBase) async {
    rust.configureDefaultBindings(libraryPath: _libraryFileName());
    final dir = await getApplicationDocumentsDirectory();
    final db = '${dir.path}${Platform.pathSeparator}wxlike.sqlite';
    return FfiWxlikeService._(WxlikeClient.create(serverBase, db));
  }

  /// 给测试与 host 侧冒烟用：直接指定库路径与数据库路径，绕开 path_provider。
  static FfiWxlikeService at(String libraryPath, String serverBase, String dbPath) {
    rust.configureDefaultBindings(libraryPath: libraryPath);
    return FfiWxlikeService._(WxlikeClient.create(serverBase, dbPath));
  }

  @override
  SessionInfo session() => _client.session();

  @override
  Future<SessionInfo> register(String name, String password) async =>
      _client.register(name, password);

  @override
  Future<SessionInfo> login(String name, String password) async => _client.login(name, password);

  @override
  Future<void> logout() async => _client.logout();

  @override
  Future<LiveInfo> connect() async => _client.connect();

  @override
  Future<void> disconnect() async => _client.disconnect();

  @override
  Future<LiveInfo> liveInfo() async => _client.liveInfo();

  @override
  Future<ConversationList> conversations() async => _client.conversations();

  @override
  Future<MessagePage> history(String conv, int beforeSeq, int limit) async =>
      _client.history(conv, beforeSeq, limit);

  @override
  Future<UnreadState> unread(String conv) async => _client.unread(conv);

  @override
  Future<UnreadState> markRead(String conv, int uptoSeq) async =>
      _client.markRead(conv, uptoSeq);

  @override
  Future<SendOutcome> sendText(String conv, String text) async => _client.sendText(conv, text);

  @override
  Future<OutboxSummary> outboxStatus() async => _client.outboxStatus();

  @override
  Future<FlushReport> flushOutbox() async => _client.flushOutbox();

  @override
  Future<OutboxSummary> retryOutbox(String taskId) async => _client.retryOutbox(taskId);

  @override
  Future<bool> dropOutbox(String taskId) async => _client.dropOutbox(taskId);

  @override
  Future<void> registerPushToken(String platform, String token) async =>
      _client.registerPushToken(platform, token);

  @override
  void close() => _client.close();
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
