import 'dart:async';

import 'package:flutter/material.dart';

import 'features/auth/auth_ports.dart';
import 'features/chat/timeline/messaging_ports.dart';
import 'platform/method_channel_platform.dart';
import 'screens/conversations.dart';
import 'screens/login.dart';
import 'shell/mobile_app.dart';
import 'src/service.dart';

/// 过渡期 A/B 开关：`flutter run --dart-define=WXLIKE_LEGACY_APP=true`
/// 跑旧壳（WxlikeApp）；默认进 G12 产品壳（WxlikeMobileApp）。
const bool _useLegacyApp = bool.fromEnvironment('WXLIKE_LEGACY_APP');

/// 默认服务器。真机/模拟器联调用
/// `--dart-define=WXLIKE_SERVER=http://10.0.2.2:8099` 覆盖；
/// 登录页"更多登录方式"里还可以再改。
const String _defaultServer = String.fromEnvironment(
  'WXLIKE_SERVER',
  defaultValue: 'http://127.0.0.1:8099',
);

void main() {
  if (_useLegacyApp) {
    runApp(const WxlikeApp());
    return;
  }
  runApp(const ProductionRoot());
}

/// 生产组合根：打开 FFI 客户端，把端口接到 [WxlikeMobileApp] 上。
///
/// 打开失败（缺 `libwxlike_ffi.so`、本地库打不开）必须显示出来而不是转圈圈——
/// 空白启动页是最难查的故障。
class ProductionRoot extends StatefulWidget {
  const ProductionRoot({super.key});

  @override
  State<ProductionRoot> createState() => _ProductionRootState();
}

class _ProductionRootState extends State<ProductionRoot> {
  final MethodChannelPlatform _platform = MethodChannelPlatform();
  final MethodChannelPushPort _push = MethodChannelPushPort();
  final WidgetsBindingLifecyclePort _lifecycle = WidgetsBindingLifecyclePort();

  FfiWxlikeService? _service;
  String? _currentBase;
  SessionInfo? _session;
  Object? _error;
  bool _opening = false;

  /// 端口对象在换服务器时保持不变：[WxlikeMobileApp] 构造期拿到的
  /// account/messaging 转发到"当前"客户端，避免换服务器后仍指向旧库。
  late final _SwitchingService _switching = _SwitchingService(this);
  late final ServiceAccountPort _account = ServiceAccountPort(_switching);
  late final FfiMessagingPort _messaging = FfiMessagingPort(_switching);

  @override
  void initState() {
    super.initState();
    unawaited(_open(_defaultServer));
  }

  @override
  void dispose() {
    _service?.close();
    _lifecycle.dispose();
    super.dispose();
  }

  /// 打开客户端。成功返回 null；失败返回原因（旧客户端保持可用）。
  Future<Object?> _open(String base) async {
    if (_opening) return StateError('正在打开客户端，请稍候');
    _opening = true;
    if (mounted) setState(() => _error = null);
    FfiWxlikeService service;
    try {
      service = await FfiWxlikeService.open(base);
    } catch (error) {
      // 打不开新库时旧客户端继续可用，登录页停在原地显示原因。
      if (mounted) setState(() => _error = error);
      return error;
    } finally {
      _opening = false;
    }
    if (!mounted) {
      service.close();
      return StateError('界面已销毁');
    }
    final previous = _service;
    setState(() {
      _service = service;
      _currentBase = base;
      _session = service.session();
    });
    // FFI 泄漏修复：换服务器必须关掉旧客户端——worker isolate、SQLite 连接
    // 与补发线程都挂在旧 service 上，不关会一直活着。
    previous?.close();
    return null;
  }

  /// 登录页"换服务器"回调：打开新客户端，返回仍指向当前客户端的账号端口。
  /// 打不开时把真实原因抛回去，账号页显示——绝不能悄悄退回旧服务器登录。
  /// 账号页每次提交都会带上服务器输入框的值；与当前服务器相同就不必
  /// 重开客户端（重开 = 换 isolate、重连 SQLite）。
  Future<AccountPort> _switchServer(String base) async {
    if (base == _currentBase) return _account;
    final error = await _open(base);
    if (error != null) throw error;
    return _account;
  }

  @override
  Widget build(BuildContext context) {
    final service = _service;
    final session = _session;
    if (service == null || session == null) {
      return MaterialApp(
        debugShowCheckedModeBanner: false,
        home: _error == null
            ? const Scaffold(body: Center(child: CircularProgressIndicator()))
            : _StartupError(
                message: describeError(_error!),
                onRetry: () => _open(_defaultServer),
              ),
      );
    }
    return WxlikeMobileApp(
      account: _account,
      session: session,
      messaging: _messaging,
      platform: _platform,
      push: _push,
      lifecycle: _lifecycle,
      secrets: _platform,
      deepLinks: _platform.deepLinks(),
      onSwitchServer: _switchServer,
    );
  }
}

/// 始终转发到"当前"客户端的 [WxlikeService]。
///
/// [ServiceAccountPort] / [FfiMessagingPort] 只在组合根构造一次；
/// 换服务器时更新 [_ProductionRootState._service]，端口与产品壳都不用重建。
class _SwitchingService implements WxlikeService {
  _SwitchingService(this._owner);

  final _ProductionRootState _owner;

  WxlikeService get _current {
    final service = _owner._service;
    if (service == null) {
      throw StateError('FFI 客户端尚未打开');
    }
    return service;
  }

  @override
  SessionInfo session() => _current.session();

  @override
  Future<SessionInfo> register(String name, String password) =>
      _current.register(name, password);

  @override
  Future<SessionInfo> login(String name, String password) =>
      _current.login(name, password);

  @override
  Future<void> logout() => _current.logout();

  @override
  Future<LiveInfo> connect() => _current.connect();

  @override
  Future<void> disconnect() => _current.disconnect();

  @override
  Future<LiveInfo> liveInfo() => _current.liveInfo();

  @override
  Future<ConversationList> conversations() => _current.conversations();

  @override
  Future<MessagePage> history(String conv, int beforeSeq, int limit) =>
      _current.history(conv, beforeSeq, limit);

  @override
  Future<UnreadState> unread(String conv) => _current.unread(conv);

  @override
  Future<UnreadState> markRead(String conv, int uptoSeq) =>
      _current.markRead(conv, uptoSeq);

  @override
  Future<SendOutcome> sendText(String conv, String text) =>
      _current.sendText(conv, text);

  @override
  Future<OutboxSummary> outboxStatus() => _current.outboxStatus();

  @override
  Future<FlushReport> flushOutbox() => _current.flushOutbox();

  @override
  Future<OutboxSummary> retryOutbox(String taskId) => _current.retryOutbox(taskId);

  @override
  Future<bool> dropOutbox(String taskId) => _current.dropOutbox(taskId);

  @override
  Future<void> registerPushToken(String platform, String token) =>
      _current.registerPushToken(platform, token);

  @override
  void close() => _owner._service?.close();
}

/// 旧壳（过渡期保留做 A/B；`--dart-define=WXLIKE_LEGACY_APP=true` 进入）。
///
/// 应用根：打开 Rust 客户端，决定进登录屏还是会话列表。
class WxlikeApp extends StatefulWidget {
  const WxlikeApp({super.key});

  @override
  State<WxlikeApp> createState() => _WxlikeAppState();
}

class _WxlikeAppState extends State<WxlikeApp> {
  static const _defaultServer = 'http://127.0.0.1:8099';

  WxlikeService? _service;
  late Future<WxlikeService> _opening = _open(_defaultServer);

  Future<WxlikeService> _open(String base) async {
    final service = await FfiWxlikeService.open(base);
    // 泄漏修复：换服务器时旧客户端（worker isolate、SQLite 连接、补发线程）
    // 必须关掉；打开失败时旧的继续可用，登录页停在原地。
    final previous = _service;
    _service = service;
    previous?.close();
    return service;
  }

  @override
  void dispose() {
    // 补发线程跟着客户端一起退（Rust 侧 Drop 里停机）
    _service?.close();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      title: 'wxlike',
      theme: ThemeData(colorSchemeSeed: const Color(0xff07c160), useMaterial3: true),
      home: FutureBuilder<WxlikeService>(
        future: _opening,
        builder: (context, snapshot) {
          if (snapshot.connectionState != ConnectionState.done) {
            return const Scaffold(body: Center(child: CircularProgressIndicator()));
          }
          if (snapshot.hasError) {
            return _StartupError(
              message: describeError(snapshot.error!),
              onRetry: () => setState(() => _opening = _open(_defaultServer)),
            );
          }
          final service = snapshot.data!;
          // session() 不联网：有 token 就直接进列表，让 connect() 去验证它还有没有效
          return service.session().signedIn
              ? ConversationsScreen(service: service, session: service.session())
              : LoginScreen(
                  service: service,
                  onSwitchServer: (base) async => _open(base),
                );
        },
      ),
    );
  }
}

class _StartupError extends StatelessWidget {
  const _StartupError({required this.message, required this.onRetry});

  final String message;
  final VoidCallback onRetry;

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      body: Center(
        child: Padding(
          padding: const EdgeInsets.all(24),
          child: Column(
            mainAxisAlignment: MainAxisAlignment.center,
            children: [
              const Icon(Icons.error_outline, size: 40),
              const SizedBox(height: 12),
              Text(message, textAlign: TextAlign.center),
              const SizedBox(height: 16),
              FilledButton(onPressed: onRetry, child: const Text('重试')),
            ],
          ),
        ),
      ),
    );
  }
}
