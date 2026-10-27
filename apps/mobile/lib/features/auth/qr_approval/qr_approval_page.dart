import 'package:flutter/material.dart';

import '../../../shell/theme.dart';
import '../../../shell/widgets.dart';
import '../../../src/service.dart';
import '../auth_controller.dart';
import '../auth_models.dart';
import '../auth_ports.dart';

/// 手机确认桌面扫码登录。
///
/// 页面只展示挑战与设备信息，并把**服务端回包里的状态**显示出来
/// （CREATED / SCANNED / APPROVED / REJECTED / EXPIRED）——批准成功不是一句
/// toast，而是状态条本身变了。离页后控制器关闭，不能再批准（对应"退出后不能
/// 继续批准"）。
///
/// `secret` 从二维码载荷解析出来，只交给控制器拼 `/scan` 的请求体；页面任何
/// 文案都不显示它，连票号形式的安全摘要也只用于日志。
class QrApprovalPage extends StatefulWidget {
  const QrApprovalPage({
    super.key,
    required this.identity,
    required this.challengeId,
    this.secret = '',
    this.session,
  });

  final IdentityPort identity;
  final String challengeId;
  final String secret;
  final SessionInfo? session;

  @override
  State<QrApprovalPage> createState() => _QrApprovalPageState();
}

class _QrApprovalPageState extends State<QrApprovalPage> {
  late final QrApprovalController _controller = QrApprovalController(
    identity: widget.identity,
    challengeId: widget.challengeId,
    secret: widget.secret,
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
    // 批准之后不再弹"成功"：成功的答案就是状态条变成 APPROVED（服务端回包）。
    // 失败才需要额外出声——状态条会显示 REJECTED/EXPIRED，但错误原因只在 banner 里。
    await _controller.approve();
  }

  Future<void> _reject() async {
    await _controller.reject();
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
              // 状态条：直接把服务端回包里的状态名显示出来。批准成功 = 这一行
              // 从 SCANNED 变成 APPROVED，而不是多出一条 toast。
              KeyedSubtree(
                key: const Key('qr-state'),
                child: G12StateChip(
                  state: challenge.contractState,
                  label: _statusText(challenge, _controller.remaining),
                  fatal: challenge.status == QrChallengeStatus.rejected ||
                      challenge.status == QrChallengeStatus.expired ||
                      (_controller.canDecide == false &&
                          challenge.status == QrChallengeStatus.pending),
                ),
              ),
              const SizedBox(height: 12),
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
              // 倒计时只在服务端真的给了到期时间时显示。扫码侧看到的回包
              // 没有 expires_at（那是建票响应才有的字段），所以这里显示
              // "由服务端裁定"而不是编一个假的剩余时间。
              if (challenge.deadlineKnown && _controller.remaining > Duration.zero)
                Text(
                  '有效期剩余 ${formatCountdown(_controller.remaining)}',
                  textAlign: TextAlign.center,
                  key: const Key('qr-countdown'),
                  style: const TextStyle(fontSize: 11, color: WxColors.textSecondary),
                )
              else if (!challenge.deadlineKnown)
                const Text(
                  '有效期由服务端裁定：过期时这里会显示真实原因',
                  textAlign: TextAlign.center,
                  style: TextStyle(fontSize: 11, color: WxColors.textSecondary),
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
          _row('发起时间', challenge.deadlineKnown ? formatClockTime(challenge.issuedAtUtc) : '未知'),
          _row('到期时间', challenge.deadlineKnown ? formatClockTime(challenge.expiresAtUtc) : '未知'),
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

  /// 状态条上的人话。状态名本身由 [QrLoginChallenge.contractState] 提供，
  /// 这里只负责"给用户看的那一句怎么说"。
  String _statusText(QrLoginChallenge challenge, Duration remaining) {
    if (challenge.status == QrChallengeStatus.pending &&
        challenge.deadlineKnown &&
        remaining == Duration.zero) {
      return '挑战已过期，请让桌面刷新二维码';
    }
    return switch (challenge.status) {
      QrChallengeStatus.pending => '桌面已出码，等待本机扫码',
      QrChallengeStatus.scanned => '本机已扫码，等待你批准',
      QrChallengeStatus.approved => '已批准，桌面端将自动登录',
      QrChallengeStatus.rejected => '已拒绝本次登录',
      QrChallengeStatus.expired => '登录码已过期，请让桌面端刷新二维码',
      QrChallengeStatus.consumed => '桌面端已完成登录',
    };
  }
}
