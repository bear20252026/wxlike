/// 「我」页：账号、应用锁、推送与退出。
///
/// 设备/会话列表的权威在身份域（G02）；本页不伪造一份本地设备清单，
/// 上游未接线时按"阻塞 + 原因"显示。
library;

import 'package:flutter/material.dart';

import '../features/auth/auth_ports.dart';
import '../platform/platform_ports.dart';
import '../src/service.dart';
import 'app_lock.dart';
import 'contracts.dart';
import 'theme.dart';
import 'widgets.dart';

class AccountPage extends StatefulWidget {
  const AccountPage({
    super.key,
    required this.account,
    required this.session,
    required this.lock,
    required this.push,
    required this.onSignOut,
  });

  final AccountPort account;
  final SessionInfo session;
  final AppLockController lock;
  final PushPort push;
  final Future<void> Function() onSignOut;

  @override
  State<AccountPage> createState() => _AccountPageState();
}

class _AccountPageState extends State<AccountPage> {
  PushToken? _token;
  String? _pushError;
  bool _pushBusy = false;
  bool _signingOut = false;

  @override
  void initState() {
    super.initState();
    widget.lock.addListener(_rebuild);
    _loadToken();
  }

  void _rebuild() {
    if (mounted) setState(() {});
  }

  Future<void> _loadToken() async {
    try {
      final token = await widget.push.token();
      if (mounted) setState(() => _token = token);
    } catch (error) {
      if (mounted) setState(() => _pushError = asG12Failure(error, operation: '读取推送 token').displayText);
    }
  }

  Future<void> _syncPush() async {
    final token = _token;
    if (token == null) {
      setState(() => _pushError = '没有可用的推送 token：请先在系统里允许通知');
      return;
    }
    setState(() {
      _pushBusy = true;
      _pushError = null;
    });
    try {
      await widget.account.registerPushToken(token.platform, token.token);
    } catch (error) {
      if (mounted) setState(() => _pushError = asG12Failure(error, operation: '登记推送').displayText);
    } finally {
      if (mounted) setState(() => _pushBusy = false);
    }
  }

  Future<void> _revokePush() async {
    setState(() {
      _pushBusy = true;
      _pushError = null;
    });
    try {
      await widget.push.revoke();
      if (mounted) setState(() => _token = null);
    } catch (error) {
      if (mounted) setState(() => _pushError = asG12Failure(error, operation: '撤销推送').displayText);
    } finally {
      if (mounted) setState(() => _pushBusy = false);
    }
  }

  Future<void> _configureLock() async {
    final controller = TextEditingController();
    final confirm = TextEditingController();
    final next = await showDialog<String>(
      context: context,
      builder: (context) => AlertDialog(
        title: Text(widget.lock.enabled ? '修改应用锁 PIN' : '设置应用锁 PIN'),
        content: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            TextField(
              key: const Key('lock-pin'),
              controller: controller,
              obscureText: true,
              keyboardType: TextInputType.number,
              maxLength: 6,
              decoration: const InputDecoration(labelText: '4–6 位数字', counterText: ''),
            ),
            TextField(
              key: const Key('lock-pin-confirm'),
              controller: confirm,
              obscureText: true,
              keyboardType: TextInputType.number,
              maxLength: 6,
              decoration: const InputDecoration(labelText: '再输入一次', counterText: ''),
            ),
          ],
        ),
        actions: [
          TextButton(onPressed: () => Navigator.pop(context), child: const Text('取消')),
          FilledButton(
            onPressed: () {
              if (!AppLockController.isValidPin(controller.text)) {
                ScaffoldMessenger.of(context).showSnackBar(const SnackBar(content: Text('PIN 需要 4–6 位数字')));
                return;
              }
              if (controller.text != confirm.text) {
                ScaffoldMessenger.of(context).showSnackBar(const SnackBar(content: Text('两次输入不一致')));
                return;
              }
              Navigator.pop(context, controller.text);
            },
            child: const Text('保存'),
          ),
        ],
      ),
    );
    controller.dispose();
    confirm.dispose();
    if (next == null) return;
    final ok = widget.lock.enabled
        ? await widget.lock.changePin(await _askCurrentPin(), next)
        : await widget.lock.setPin(next);
    if (!ok && mounted) {
      ScaffoldMessenger.of(context).showSnackBar(
        SnackBar(content: Text(widget.lock.error ?? '设置应用锁失败')),
      );
    }
  }

  Future<String> _askCurrentPin() async {
    if (!widget.lock.enabled) return '';
    final input = TextEditingController();
    final pin = await showDialog<String>(
      context: context,
      builder: (context) => AlertDialog(
        title: const Text('输入当前 PIN'),
        content: TextField(
          key: const Key('lock-pin-current'),
          controller: input,
          obscureText: true,
          keyboardType: TextInputType.number,
          maxLength: 6,
          decoration: const InputDecoration(counterText: ''),
        ),
        actions: [
          TextButton(onPressed: () => Navigator.pop(context), child: const Text('取消')),
          FilledButton(onPressed: () => Navigator.pop(context, input.text), child: const Text('确认')),
        ],
      ),
    );
    input.dispose();
    return pin ?? '';
  }

  Future<void> _disableLock() async {
    final pin = await _askCurrentPin();
    if (pin.isEmpty) return;
    final ok = await widget.lock.disable(pin);
    if (!ok && mounted) {
      ScaffoldMessenger.of(context).showSnackBar(
        SnackBar(content: Text(widget.lock.error ?? '关闭应用锁失败')),
      );
    }
  }

  Future<void> _signOut() async {
    final confirmed = await showDialog<bool>(
      context: context,
      builder: (context) => AlertDialog(
        title: const Text('退出登录？'),
        content: const Text('本机缓存的消息仍保留在本地库；重新登录后可继续同步。'),
        actions: [
          TextButton(onPressed: () => Navigator.pop(context, false), child: const Text('取消')),
          FilledButton(onPressed: () => Navigator.pop(context, true), child: const Text('退出')),
        ],
      ),
    );
    if (confirmed != true || _signingOut) return;
    setState(() => _signingOut = true);
    try {
      await widget.onSignOut();
    } finally {
      if (mounted) setState(() => _signingOut = false);
    }
  }

  @override
  Widget build(BuildContext context) {
    final dark = Theme.of(context).brightness == Brightness.dark;
    return ListView(
      padding: const EdgeInsets.only(bottom: 32),
      children: [
        Container(
          color: dark ? WxColors.darkSurface : WxColors.surface,
          padding: const EdgeInsets.fromLTRB(16, 20, 16, 20),
          child: Row(
            children: [
              CircleAvatar(
                radius: 28,
                backgroundColor: WxColors.brandSoft,
                child: Text(
                  widget.session.userId.isEmpty ? '?' : widget.session.userId.substring(0, 1).toUpperCase(),
                  style: const TextStyle(color: WxColors.brandDark, fontSize: 22, fontWeight: FontWeight.w600),
                ),
              ),
              const SizedBox(width: 14),
              Expanded(
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    Text(widget.session.userId, style: const TextStyle(fontSize: 17, fontWeight: FontWeight.w600)),
                    const SizedBox(height: 4),
                    Text(
                      widget.session.server,
                      style: TextStyle(fontSize: 12, color: dark ? WxColors.darkTextSecondary : WxColors.textSecondary),
                    ),
                  ],
                ),
              ),
            ],
          ),
        ),
        const SizedBox(height: 12),
        _group([
          ListTile(
            leading: const Icon(Icons.lock_outline),
            title: const Text('应用锁'),
            subtitle: Text(
              widget.lock.enabled
                  ? '已开启 · 后台超过 1 分钟重新锁定'
                  // 安全存储不可用时锁被停用（见 AppLockController.restore）：
                  // 警告必须可见，不能只留一个"未开启"。
                  : (widget.lock.error != null ? '未开启 · ${widget.lock.error}' : '未开启'),
            ),
            trailing: Switch(
              value: widget.lock.enabled,
              onChanged: widget.lock.busy
                  ? null
                  : (value) {
                      if (value) {
                        _configureLock();
                      } else {
                        _disableLock();
                      }
                    },
            ),
            onTap: widget.lock.busy ? null : _configureLock,
          ),
          ListTile(
            leading: const Icon(Icons.lock_clock),
            title: const Text('立即锁定'),
            enabled: widget.lock.enabled,
            onTap: widget.lock.enabled ? widget.lock.lockNow : null,
          ),
        ]),
        const SizedBox(height: 12),
        _group([
          ListTile(
            leading: const Icon(Icons.notifications_active_outlined),
            title: const Text('推送 token'),
            subtitle: Text(
              _token == null ? '未取得（需要系统通知权限）' : '${_token!.platform} · ${_mask(_token!.token)}',
            ),
            trailing: _pushBusy
                ? const SizedBox(width: 18, height: 18, child: CircularProgressIndicator(strokeWidth: 2))
                : TextButton(
                    onPressed: _syncPush,
                    child: const Text('登记'),
                  ),
          ),
          ListTile(
            leading: const Icon(Icons.notifications_off_outlined),
            title: const Text('撤销本机推送'),
            subtitle: const Text('退出登录前应撤销；服务端未提供删除端点时会明确报错'),
            enabled: !_pushBusy,
            onTap: _revokePush,
          ),
          if (_pushError != null)
            Padding(
              padding: const EdgeInsets.fromLTRB(16, 0, 16, 12),
              child: G12ErrorBanner(message: _pushError!),
            ),
        ]),
        const SizedBox(height: 12),
        _group([
          const ListTile(
            leading: Icon(Icons.devices_outlined),
            title: Text('登录设备'),
            subtitle: Text('identity.session.list / identity.device.list 尚未接入 G02 绑定'),
          ),
        ]),
        const SizedBox(height: 24),
        Padding(
          padding: const EdgeInsets.symmetric(horizontal: 16),
          child: OutlinedButton(
            onPressed: _signingOut ? null : _signOut,
            style: OutlinedButton.styleFrom(foregroundColor: WxColors.danger),
            child: _signingOut
                ? const SizedBox(height: 18, child: CircularProgressIndicator(strokeWidth: 2))
                : const Text('退出登录'),
          ),
        ),
      ],
    );
  }

  Widget _group(List<Widget> children) {
    return Container(
      color: Theme.of(context).brightness == Brightness.dark ? WxColors.darkSurface : WxColors.surface,
      child: Column(children: children),
    );
  }

  static String _mask(String token) {
    if (token.length <= 10) return '••••';
    return '${token.substring(0, 4)}••••${token.substring(token.length - 4)}';
  }
}
