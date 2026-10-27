/// 「扫码登录」入口：把二维码里的载荷交给解析器，解析通过才进确认页。
///
/// 相机取帧与解码由平台桥提供（G12-05 的 `PlatformPort.takePhoto/pickPhoto` +
/// 一个真正的解码器），本卡不伪造那条链路；这里收的是**已经拿到的载荷字符串**
/// （扫码桥回调、深链、或用户粘贴），并把"能不能进确认页"完全交给
/// [QrScanPayload.parse]：不合形状就地显示原因，绝不带着半截凭据去发请求。
library;

import 'dart:async';

import 'package:flutter/material.dart';

import '../../../shell/contracts.dart';
import '../../../shell/theme.dart';
import '../../../shell/widgets.dart';
import '../../../src/service.dart';
import '../auth_ports.dart';
import 'qr_approval_page.dart';
import 'qr_payload.dart';

class QrScanLoginPage extends StatefulWidget {
  const QrScanLoginPage({
    super.key,
    required this.identity,
    this.session,
    this.initialPayload = '',
  });

  final IdentityPort identity;
  final SessionInfo? session;
  final String initialPayload;

  @override
  State<QrScanLoginPage> createState() => _QrScanLoginPageState();
}

class _QrScanLoginPageState extends State<QrScanLoginPage> {
  late final TextEditingController _input =
      TextEditingController(text: widget.initialPayload);
  String? _error;

  @override
  void dispose() {
    _input.dispose();
    super.dispose();
  }

  /// 解析 → 跳转。错误只在这里落地成一行可见文案（不含 secret）。
  void _submit() {
    final raw = _input.text;
    try {
      final payload = QrScanPayload.parse(raw);
      unawaited(Navigator.of(context).push(
        MaterialPageRoute<void>(
          builder: (_) => QrApprovalPage(
            identity: widget.identity,
            challengeId: payload.ticketId,
            secret: payload.secret,
            session: widget.session,
          ),
        ),
      ));
    } on G12Failure catch (failure) {
      // displayText 由解析器给出，文案里只可能出现票号与 `<redacted>`。
      setState(() => _error = failure.displayText);
    }
  }

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(title: const Text('扫码登录')),
      body: SafeArea(
        child: ListView(
          padding: const EdgeInsets.all(20),
          children: [
            const Text(
              '扫描电脑上 wxlike 登录二维码',
              style: TextStyle(fontSize: 16, fontWeight: FontWeight.w600),
            ),
            const SizedBox(height: 6),
            const Text(
              '相机解码由平台桥提供；也可以把二维码内容（形如 '
              '${QrScanPayload.scheme}://…）粘贴在下面。密钥只在一次请求里使用，'
              '不会保存或写进日志。',
              style: TextStyle(fontSize: 11, height: 1.5, color: WxColors.textSecondary),
            ),
            const SizedBox(height: 16),
            TextField(
              key: const Key('qr-login-payload'),
              controller: _input,
              maxLines: 2,
              decoration: const InputDecoration(
                labelText: '二维码内容',
                hintText: '${QrScanPayload.scheme}://票号/密钥',
                border: OutlineInputBorder(),
              ),
              onChanged: (_) {
                if (_error != null) setState(() => _error = null);
              },
            ),
            const SizedBox(height: 12),
            if (_error != null) ...[
              G12ErrorBanner(message: _error!),
              const SizedBox(height: 12),
            ],
            FilledButton(
              key: const Key('qr-login-submit'),
              onPressed: _submit,
              child: const Text('下一步'),
            ),
          ],
        ),
      ),
    );
  }
}
