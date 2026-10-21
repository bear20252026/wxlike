import 'package:flutter/material.dart';

import 'screens/conversations.dart';
import 'screens/login.dart';
import 'src/service.dart';

void main() => runApp(const WxlikeApp());

/// 应用根：打开 Rust 客户端，决定进登录屏还是会话列表。
///
/// "打开失败"必须显示出来而不是转圈圈——加载不到 `libwxlike_ffi.so` 或本地库
/// 打不开时，一个空白启动页是最难查的故障。
class WxlikeApp extends StatefulWidget {
  const WxlikeApp({super.key});

  @override
  State<WxlikeApp> createState() => _WxlikeAppState();
}

class _WxlikeAppState extends State<WxlikeApp> {
  static const _defaultServer = 'http://127.0.0.1:8099';

  final List<WxlikeService> _opened = [];
  late Future<WxlikeService> _opening = _open(_defaultServer);

  Future<WxlikeService> _open(String base) async {
    final service = await FfiWxlikeService.open(base);
    _opened.add(service);
    return service;
  }

  @override
  void dispose() {
    // 补发线程跟着客户端一起退（Rust 侧 Drop 里停机）
    for (final service in _opened) {
      service.close();
    }
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
