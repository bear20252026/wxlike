import 'package:flutter/material.dart';

import '../../../shell/theme.dart';
import '../../../shell/widgets.dart';
import '../../../src/service.dart';
import '../auth_controller.dart';
import '../auth_models.dart';
import '../auth_ports.dart';

/// 手机确认桌面扫码登录。
///
/// 页面只展示挑战与设备信息；批准/拒绝后由 Rust 完成密钥与 token 交接，
/// Dart 不保存 token。离页后控制器关闭，不能再批准（对应"退出后不能继续批准"）。
class QrApprovalPage extends StatefulWidget {
  const QrApprovalPage({
    super.key,
    required this.identity,
    required this.challengeId,
    this.session,
  });

  final IdentityPort identity;
  final String challengeId;
  final SessionInfo? session;

  @override
  State<QrApprovalPage> createState() => _QrApprovalPageState();
}

class _QrApprovalPageState extends State<QrApprovalPage> {
  late final QrApprovalController _controller = QrApprovalController(
    identity: widget.identity,
    challengeId: widget.challengeId,
  );

  @override
  void initState() {
    super.initState();
    _controller.addListener(_rebuild);
    _controller.load();
  }

  void _rebuild() {
    if (mounted) setState(() {});
  }

  @override
  void dispose() {
    _controller.removeListener(_rebuild);
    _controller.dispose();
    super.dispose();
  }

  Future<void> _approve() async {
    final challenge = _controller.challenge;
    if (challenge == null) return;
    final confirmed = await showDialog<bool>(
      context: context,
      builder: (context) => AlertDialog(
        title: const Text('确认在这台设备登录？'),
        content: Text(
          '设备：${challenge.deviceName}\n类型：${challenge.deviceKind}\n位置：${challenge.location}\n\n'
          '仅批准这一台桌面；其他设备不会获得登录态。',
        ),
        actions: [
          TextButton(onPressed: () => Navigator.pop(context, false), child: const Text('取消')),
          FilledButton(onPressed: () => Navigator.pop(context, true), child: const Text('批准登录')),
        ],
      ),
    );
    if (confirmed != true || !mounted) return;
    final ok = await _controller.approve();
    if (!mounted) return;
    ScaffoldMessenger.of(context).showSnackBar(
      SnackBar(content: Text(ok ? '已批准，桌面端将自动登录' : (_controller.failure?.displayText ?? '批准失败'))),
    );
  }

  Future<void> _reject() async {
    final ok = await _controller.reject();
    if (!mounted) return;
    ScaffoldMessenger.of(context).showSnackBar(
      SnackBar(content: Text(ok ? '已拒绝本次登录' : (_controller.failure?.displayText ?? '拒绝失败'))),
    );
  }

  @override
  Widget build(BuildContext context) {
    final challenge = _controller.challenge;
    return Scaffold(
      appBar: AppBar(title: const Text('设备登录确认')),
      body: SafeArea(
        child: ListView(
          padding: const EdgeInsets.all(20),
          children: [
            if (_controller.loading && challenge == null)
              const Center(child: Padding(padding: EdgeInsets.all(32), child: CircularProgressIndicator()))
            else if (_controller.failure != null && challenge == null)
              G12BlockedState(
                title: '无法读取登录挑战',
                reason: _controller.failure!.displayText,
                onRetry: _controller.load,
              )
            else if (challenge != null) ...[
              Icon(Icons.laptop_mac, size: 56, color: WxColors.brand),
              const SizedBox(height: 12),
              Text(
                challenge.deviceName,
                textAlign: TextAlign.center,
                style: const TextStyle(fontSize: 18, fontWeight: FontWeight.w600),
              ),
              const SizedBox(height: 6),
              Text(
                '${challenge.deviceKind} · ${challenge.location}',
                textAlign: TextAlign.center,
                style: const TextStyle(fontSize: 12, color: WxColors.textSecondary),
              ),
              const SizedBox(height: 24),
              _card(challenge),
              const SizedBox(height: 16),
              if (_controller.remaining > Duration.zero && challenge.isPending)
                Text(
                  '有效期剩余 ${formatCountdown(_controller.remaining)}',
                  textAlign: TextAlign.center,
                  style: const TextStyle(fontSize: 11, color: WxColors.textSecondary),
                )
              else
                Text(
                  _statusText(challenge.status, _controller.remaining),
                  textAlign: TextAlign.center,
                  style: const TextStyle(fontSize: 11, color: WxColors.danger),
                ),
              const SizedBox(height: 24),
              if (_controller.failure != null)
                G12ErrorBanner(message: _controller.failure!.displayText),
              const SizedBox(height: 8),
              FilledButton(
                key: const Key('qr-approve'),
                onPressed: _controller.canDecide && !_controller.deciding ? _approve : null,
                child: _controller.deciding
                    ? const SizedBox(width: 18, height: 18, child: CircularProgressIndicator(strokeWidth: 2))
                    : const Text('批准登录'),
              ),
              const SizedBox(height: 8),
              OutlinedButton(
                key: const Key('qr-reject'),
                onPressed: _controller.canDecide && !_controller.deciding ? _reject : null,
                child: const Text('拒绝'),
              ),
              const SizedBox(height: 20),
              const Text(
                '批准后新设备会获得这个账号的登录态。若不是你本人操作，请拒绝并修改密码。',
                textAlign: TextAlign.center,
                style: TextStyle(fontSize: 10, height: 1.5, color: WxColors.textSecondary),
              ),
            ],
          ],
        ),
      ),
    );
  }

  Widget _card(QrLoginChallenge challenge) {
    return Container(
      padding: const EdgeInsets.all(16),
      decoration: BoxDecoration(
        color: Theme.of(context).colorScheme.surface,
        borderRadius: BorderRadius.circular(WxRadius.card),
      ),
      child: Column(
        children: [
          _row('挑战', challenge.id),
          _row('发起时间', formatClockTime(challenge.issuedAtUtc)),
          _row('到期时间', formatClockTime(challenge.expiresAtUtc)),
          if (challenge.scannedBy.isNotEmpty) _row('扫码账号', challenge.scannedBy),
          _row('授权范围', '仅目标桌面设备'),
        ],
      ),
    );
  }

  Widget _row(String label, String value) {
    return Padding(
      padding: const EdgeInsets.symmetric(vertical: 6),
      child: Row(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          SizedBox(
            width: 72,
            child: Text(label, style: const TextStyle(fontSize: 11, color: WxColors.textSecondary)),
          ),
          Expanded(
            child: Text(
              value,
              textAlign: TextAlign.right,
              style: const TextStyle(fontSize: 11),
            ),
          ),
        ],
      ),
    );
  }

  String _statusText(QrChallengeStatus status, Duration remaining) {
    if (status == QrChallengeStatus.pending && remaining == Duration.zero) return '挑战已过期，请让桌面刷新二维码';
    return switch (status) {
      QrChallengeStatus.pending => '等待确认',
      QrChallengeStatus.approved => '已批准',
      QrChallengeStatus.rejected => '已拒绝',
      QrChallengeStatus.expired => '已过期',
      QrChallengeStatus.consumed => '已完成登录',
    };
  }
}
