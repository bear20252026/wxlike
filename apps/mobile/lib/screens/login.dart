import 'package:flutter/material.dart';

import '../src/service.dart';
import '../src/widgets.dart';
import 'conversations.dart';

/// 登录 / 注册屏。
///
/// 服务器地址是可以改的：真机上的 `127.0.0.1` 指的是手机自己，验收时几乎一定要
/// 填开发机的局域网地址。这个输入框就是为那一步准备的。
class LoginScreen extends StatefulWidget {
  const LoginScreen({
    super.key,
    required this.service,
    this.onSwitchServer,
  });

  final WxlikeService service;

  /// 换了地址就要换客户端；为 null 时输入框只读。
  final Future<WxlikeService> Function(String base)? onSwitchServer;

  @override
  State<LoginScreen> createState() => _LoginScreenState();
}

class _LoginScreenState extends State<LoginScreen> {
  final _server = TextEditingController(text: 'http://127.0.0.1:8099');
  final _name = TextEditingController();
  final _password = TextEditingController();

  String? _error;
  bool _busy = false;

  @override
  void dispose() {
    _server.dispose();
    _name.dispose();
    _password.dispose();
    super.dispose();
  }

  Future<void> _run(bool login) async {
    setState(() {
      _busy = true;
      _error = null;
    });
    try {
      var service = widget.service;
      final base = _server.text.trim();
      if (base.isNotEmpty && widget.onSwitchServer != null) {
        service = await widget.onSwitchServer!(base);
      }
      final name = _name.text.trim();
      final session = login
          ? await service.login(name, _password.text)
          : await service.register(name, _password.text);
      if (!mounted) return;
      Navigator.of(context).pushReplacement(
        MaterialPageRoute(
          builder: (_) => ConversationsScreen(service: service, session: session),
        ),
      );
    } catch (error) {
      if (mounted) setState(() => _error = describeError(error));
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  @override
  Widget build(BuildContext context) {
    final ready = _name.text.trim().isNotEmpty && _password.text.isNotEmpty && !_busy;
    return Scaffold(
      appBar: AppBar(title: const Text('登录 wxlike')),
      body: ListView(
        padding: const EdgeInsets.all(16),
        children: [
          TextField(
            controller: _server,
            enabled: widget.onSwitchServer != null && !_busy,
            keyboardType: TextInputType.url,
            decoration: const InputDecoration(
              labelText: '服务器地址',
              helperText: '真机上填开发机的局域网地址，127.0.0.1 是手机自己',
              border: OutlineInputBorder(),
            ),
          ),
          const SizedBox(height: 12),
          TextField(
            controller: _name,
            onChanged: (_) => setState(() {}),
            decoration: const InputDecoration(
              labelText: '用户名',
              border: OutlineInputBorder(),
            ),
          ),
          const SizedBox(height: 12),
          TextField(
            controller: _password,
            onChanged: (_) => setState(() {}),
            obscureText: true,
            decoration: const InputDecoration(
              labelText: '密码',
              border: OutlineInputBorder(),
            ),
          ),
          if (_error != null) ...[
            const SizedBox(height: 16),
            ErrorBox(message: _error!),
          ],
          const SizedBox(height: 20),
          FilledButton(
            onPressed: ready ? () => _run(true) : null,
            child: _busy
                ? const SizedBox(height: 18, child: CircularProgressIndicator(strokeWidth: 2))
                : const Text('登录'),
          ),
          const SizedBox(height: 8),
          OutlinedButton(onPressed: ready ? () => _run(false) : null, child: const Text('注册新账号')),
        ],
      ),
    );
  }
}
